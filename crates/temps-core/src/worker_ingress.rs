// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct worker ingress (ADR 058 D7): what the agent and the control plane
//! must agree on.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// Path prefix the worker's public HTTP listener answers reachability checks
/// on, before any host routing: `{PROBE_PATH_PREFIX}{nonce}`.
pub const PROBE_PATH_PREFIX: &str = "/.well-known/temps-ingress/probe/";

/// Length of a probe nonce, in hex characters. Fixed so an answer for one
/// nonce cannot be extended into an answer for another.
pub const PROBE_NONCE_LEN: usize = 32;

/// The answer to a reachability check: a digest of the nonce keyed on the
/// node's token hash (`sha256_hex(node_token)`), which only the node and the
/// control plane hold.
pub fn probe_answer(token_hash_hex: &str, nonce: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"temps-ingress-probe\0");
    digest.update(token_hash_hex.as_bytes());
    digest.update(b"\0");
    digest.update(nonce.as_bytes());
    hex::encode(digest.finalize())
}

/// Whether `nonce` has the shape the control plane sends.
pub fn is_probe_nonce(nonce: &str) -> bool {
    nonce.len() == PROBE_NONCE_LEN && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A fresh X25519 key pair for certificate bundles, base64 `(private, public)`:
/// the same shape `temps join` enrolls.
pub fn generate_ingress_key() -> Result<(String, String), crate::ecies::EciesError> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let secret = crate::ecies::generate_x25519_static_secret()?;
    let public = x25519_dalek::PublicKey::from(&secret);
    Ok((
        STANDARD.encode(secret.as_bytes()),
        STANDARD.encode(public.as_bytes()),
    ))
}

/// Who can reach an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressScope {
    /// Reachable from the internet, if nothing filters it.
    Public,
    /// RFC 1918, IPv6 unique-local: behind a router, needs a port forward.
    Private,
    /// RFC 6598 carrier-grade NAT: the provider's NAT, no port forward possible.
    Cgnat,
    /// Loopback, link-local, unspecified, multicast or documentation ranges.
    Local,
}

pub fn address_scope(ip: IpAddr) -> AddressScope {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
            {
                AddressScope::Local
            } else if a == 100 && (64..128).contains(&b) {
                AddressScope::Cgnat
            } else if v4.is_private() {
                AddressScope::Private
            } else {
                AddressScope::Public
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return address_scope(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            // Loopback, unspecified, multicast, link-local and documentation.
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || first & 0xffc0 == 0xfe80
                || (first == 0x2001 && v6.segments()[1] == 0x0db8)
            {
                AddressScope::Local
            } else if first & 0xfe00 == 0xfc00 {
                AddressScope::Private
            } else {
                AddressScope::Public
            }
        }
    }
}

/// What a worker lets its control plane do with public ingress. Set on the
/// worker only (`temps join --ingress`, `temps agent ingress`): opening ports
/// on a machine is its owner's decision, whoever runs the control plane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngressMode {
    /// No public listeners.
    #[default]
    Off,
    /// The worker chose where to listen; the control plane routes to it.
    Fixed,
    /// The control plane chooses where to listen, among the worker's own
    /// addresses, and turns the listeners on and off.
    Delegated,
}

impl IngressMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IngressMode::Off => "off",
            IngressMode::Fixed => "fixed",
            IngressMode::Delegated => "delegated",
        }
    }
}

impl std::str::FromStr for IngressMode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(IngressMode::Off),
            "fixed" => Ok(IngressMode::Fixed),
            "delegated" => Ok(IngressMode::Delegated),
            other => Err(format!(
                "unknown ingress mode '{other}': use off, fixed or delegated"
            )),
        }
    }
}

/// The worker's own ingress setting, saved in `agent.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalIngress {
    pub mode: IngressMode,
    /// `fixed` only: the address to bind; `None` means the default-route
    /// interface.
    #[serde(default)]
    pub listen: Option<IpAddr>,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_https_port")]
    pub https_port: u16,
}

fn default_http_port() -> u16 {
    80
}

fn default_https_port() -> u16 {
    443
}

impl Default for LocalIngress {
    fn default() -> Self {
        Self {
            mode: IngressMode::Off,
            listen: None,
            http_port: 80,
            https_port: 443,
        }
    }
}

/// One host address the agent reports as a candidate listen address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportedInterface {
    pub name: String,
    pub address: String,
    /// The interface holding the default route: what "automatic" binds.
    #[serde(default)]
    pub default_route: bool,
}

/// What the agent bound for direct ingress, reported to the control plane.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListenerReport {
    /// Host addresses the operator can choose from.
    pub interfaces: Vec<ReportedInterface>,
    /// `ip:port` actually bound, when bound.
    pub http: Option<String>,
    pub https: Option<String>,
    /// The worker's ingress mode; `None` from agents that predate modes.
    pub mode: Option<IngressMode>,
    /// Why the listener is not bound as configured.
    pub error: Option<String>,
}
