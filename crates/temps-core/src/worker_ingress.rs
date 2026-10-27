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

/// Parses an address the agent reported, bare (`203.0.113.7`) or with a
/// port (`203.0.113.7:80`).
pub fn parse_reported_ip(value: &str) -> Option<IpAddr> {
    value.parse::<IpAddr>().ok().or_else(|| {
        value
            .parse::<std::net::SocketAddr>()
            .ok()
            .map(|address| address.ip())
    })
}

/// The address DNS should point at for a worker's direct ingress: the
/// configured public address, else the address the agent actually bound,
/// else the configured listen address, else the default-route interface the
/// agent reported.
pub fn effective_public_address(
    public_address: Option<&str>,
    listening_http: Option<&str>,
    listen_address: Option<&str>,
    interfaces: &[ReportedInterface],
) -> Option<IpAddr> {
    public_address
        .and_then(parse_reported_ip)
        .or_else(|| listening_http.and_then(parse_reported_ip))
        .or_else(|| listen_address.and_then(parse_reported_ip))
        .or_else(|| {
            interfaces
                .iter()
                .find(|i| i.default_route)
                .and_then(|i| parse_reported_ip(&i.address))
        })
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
    /// Listen only on a private address the worker chose (ADR 064 D5): a
    /// tailnet or LAN address, port 443, and 80 only to redirect to it.
    /// Serves only addresses whose audience is `private`.
    Private,
}

impl IngressMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IngressMode::Off => "off",
            IngressMode::Fixed => "fixed",
            IngressMode::Delegated => "delegated",
            IngressMode::Private => "private",
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
            "private" => Ok(IngressMode::Private),
            other => Err(format!(
                "unknown ingress mode '{other}': use off, fixed, delegated or private"
            )),
        }
    }
}

/// The worker's own ingress setting, saved in `agent.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalIngress {
    pub mode: IngressMode,
    /// `fixed`: the address to bind; `None` means the default-route
    /// interface. `private`: the private address to bind, unless
    /// `interface` names the interface to take it from.
    #[serde(default)]
    pub listen: Option<IpAddr>,
    /// `private` only: bind this interface's address, looked up each time
    /// the listener starts (`tailscale0` for `--listen tailscale`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_https_port")]
    pub https_port: u16,
    /// The worker owner allows the agent to drive tailscaled (ADR 065 D2):
    /// Serve entries for tailnet names, `AdvertiseServices`, status and
    /// whois. Without it the agent never opens tailscaled's socket.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tailscale_serve: bool,
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
            interface: None,
            http_port: 80,
            https_port: 443,
            tailscale_serve: false,
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
    /// Tailscale's interface (`tailscale0`, an address in `100.64.0.0/10`):
    /// what `--listen tailscale` binds (ADR 064 D5).
    #[serde(default)]
    pub tailscale: bool,
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
    /// `private` mode: the worker's inside checks of the private addresses
    /// it serves (ADR 064 D7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_checks: Vec<PrivateCheck>,
    /// What the agent found about tailscaled and did with its Serve entries
    /// (ADR 065 D2, D4). `None` from agents that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale: Option<crate::tailnet::TailscaleReport>,
    /// What this agent enforces, e.g. `gate.v1` (ADR 065 D7) or
    /// `tailnet_identity.v1` (D6). Informational: what a snapshot may carry
    /// is decided from what each request declares, never from this report.
    /// An agent that predates the field reports none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// The port a private listener serves HTTPS on, always (ADR 064 D5): a
/// private address is a plain `https://<hostname>` URL.
pub const PRIVATE_HTTPS_PORT: u16 = 443;
/// The port a private listener answers plain HTTP on, only to redirect to
/// HTTPS.
pub const PRIVATE_HTTP_PORT: u16 = 80;
/// Tailscale's interface on Linux, which `--listen tailscale` binds.
pub const TAILSCALE_INTERFACE: &str = "tailscale0";
/// The range Tailscale assigns node addresses from (carrier-grade NAT).
pub const TAILSCALE_RANGE: &str = "100.64.0.0/10";

/// The private block `ip` sits in, which bounds who a private address
/// admits: `100.64.0.0/10` (Tailscale and other CGNAT overlays), one of
/// RFC 1918's, or `fd00::/8`. `None` for any other address.
pub fn private_block(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            match (a, b) {
                (100, 64..=127) => Some(TAILSCALE_RANGE),
                (10, _) => Some("10.0.0.0/8"),
                (172, 16..=31) => Some("172.16.0.0/12"),
                (192, 168) => Some("192.168.0.0/16"),
                _ => None,
            }
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => private_block(IpAddr::V4(v4)),
            None if v6.octets()[0] == 0xfd => Some("fd00::/8"),
            None => None,
        },
    }
}

/// Whether a private listener may bind `ip`, and the range it admits. The
/// worker refuses anything else, and so does the control plane, so a
/// private address is never answered on a public one.
pub fn check_private_listen(ip: IpAddr) -> Result<&'static str, String> {
    const ALLOWED: &str = "an address in 100.64.0.0/10 (Tailscale), 10.0.0.0/8, 172.16.0.0/12, \
                           192.168.0.0/16 or fd00::/8";
    if ip.is_unspecified() {
        return Err(format!(
            "{ip} would listen on every address of this machine, its public ones too. A private \
             listener binds one private address: use `--listen tailscale`, or {ALLOWED}."
        ));
    }
    if ip.is_loopback() {
        return Err(format!(
            "{ip} is loopback: nobody else on your network could reach it. Use `--listen \
             tailscale`, or {ALLOWED}."
        ));
    }
    private_block(ip).ok_or_else(|| {
        format!(
            "{ip} is not a private address, and a private listener never binds one anybody on \
             the internet can reach. Use `--listen tailscale`, or {ALLOWED}. For public traffic, \
             use `temps agent ingress fixed` or `delegated` instead."
        )
    })
}

/// Whether `ip` is inside `range` (`100.64.0.0/10`, or a bare address).
/// `false` when the range does not parse, so a broken range admits nobody.
pub fn ip_in_range(ip: IpAddr, range: &str) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    let range = range.trim();
    if let Ok(net) = range.parse::<ipnet::IpNet>() {
        return net.contains(&ip);
    }
    range.parse::<IpAddr>().is_ok_and(|single| single == ip)
}

/// The id a private address goes by between the control plane and the
/// worker (ADR 064 D7): the worker's inside check takes only this, and maps
/// it through its own route snapshot. `custom:<id>` for a custom domain,
/// `environment:<id>` for an environment domain.
pub fn private_address_id(custom_domain: bool, id: i32) -> String {
    if custom_domain {
        format!("custom:{id}")
    } else {
        format!("environment:{id}")
    }
}

/// How a worker's inside check of a private address came out (ADR 064 D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrivateCheckOutcome {
    /// The hostname resolves to the listener, and it answered over HTTPS
    /// with a certificate valid for the hostname.
    Reachable,
    /// The hostname does not resolve from the worker.
    NotResolved,
    /// It resolves somewhere other than the recorded listener; nothing was
    /// dialled.
    ResolvesElsewhere,
    /// The listener is not bound, or refused the connection.
    NotListening,
    /// The TLS handshake failed or the certificate is not valid for the
    /// hostname.
    Certificate,
    /// The listener answered, but not with this address's route.
    NoRoute,
}

/// One inside check, as the worker reports it. The control plane stamps
/// it with the time it received the report, never the worker's clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PrivateCheck {
    pub address_id: String,
    pub hostname: String,
    /// The listener address checked against, `ip`.
    pub listener: String,
    pub outcome: PrivateCheckOutcome,
    /// What happened, in words, with what to do when it failed.
    pub message: String,
    /// The HTTP status the listener answered with, when it answered.
    #[serde(default)]
    pub status: Option<u16>,
}

#[cfg(test)]
mod effective_address_tests {
    use super::*;

    #[test]
    fn configured_public_address_wins_then_bound_then_interface() {
        let interfaces = vec![ReportedInterface {
            name: "eth0".into(),
            address: "198.51.100.4".into(),
            default_route: true,
            tailscale: false,
        }];
        assert_eq!(
            effective_public_address(Some("203.0.113.7"), Some("10.0.0.2:80"), None, &interfaces),
            "203.0.113.7".parse().ok()
        );
        assert_eq!(
            effective_public_address(None, Some("10.0.0.2:80"), None, &interfaces),
            "10.0.0.2".parse().ok()
        );
        assert_eq!(
            effective_public_address(None, None, None, &interfaces),
            "198.51.100.4".parse().ok()
        );
        assert_eq!(effective_public_address(None, None, None, &[]), None);
    }
}

#[cfg(test)]
mod private_tests {
    use super::*;

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn a_private_listener_binds_only_a_private_address() {
        assert_eq!(
            check_private_listen(ip("100.101.102.103")),
            Ok(TAILSCALE_RANGE)
        );
        assert_eq!(check_private_listen(ip("10.0.0.9")), Ok("10.0.0.0/8"));
        assert_eq!(check_private_listen(ip("172.20.1.1")), Ok("172.16.0.0/12"));
        assert_eq!(
            check_private_listen(ip("192.168.1.8")),
            Ok("192.168.0.0/16")
        );
        assert_eq!(
            check_private_listen(ip("fd7a:115c:a1e0::1")),
            Ok("fd00::/8")
        );
        for (refused, why) in [
            ("0.0.0.0", "every address"),
            ("::", "every address"),
            ("127.0.0.1", "loopback"),
            ("203.0.113.7", "not a private address"),
            ("8.8.8.8", "not a private address"),
            ("2001:4860::8888", "not a private address"),
            // fc00::/8 is unique-local too, but not the half D5 allows.
            ("fc00::1", "not a private address"),
            ("172.32.0.1", "not a private address"),
            ("100.128.0.1", "not a private address"),
        ] {
            let error = check_private_listen(ip(refused)).unwrap_err();
            assert!(error.contains(why), "{refused}: {error}");
            assert!(error.contains("--listen tailscale"), "{refused}: {error}");
        }
    }

    #[test]
    fn a_range_admits_only_its_own_addresses() {
        assert!(ip_in_range(ip("100.90.1.2"), "100.64.0.0/10"));
        assert!(!ip_in_range(ip("203.0.113.7"), "100.64.0.0/10"));
        assert!(ip_in_range(ip("::ffff:100.90.1.2"), "100.64.0.0/10"));
        assert!(ip_in_range(ip("192.168.1.8"), "192.168.1.8"));
        assert!(!ip_in_range(ip("192.168.1.8"), "not a range"));
    }

    #[test]
    fn private_mode_round_trips_with_its_interface() {
        let saved = LocalIngress {
            mode: IngressMode::Private,
            interface: Some(TAILSCALE_INTERFACE.into()),
            ..LocalIngress::default()
        };
        let text = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<LocalIngress>(&text).unwrap(), saved);
        // Settings written before private mode still read.
        let old: LocalIngress = serde_json::from_str(
            r#"{"mode":"fixed","listen":"10.0.0.2","http_port":80,"https_port":443}"#,
        )
        .unwrap();
        assert_eq!(old.interface, None);
        assert_eq!("private".parse::<IngressMode>(), Ok(IngressMode::Private));
    }
}
