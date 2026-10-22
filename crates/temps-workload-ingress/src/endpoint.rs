// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The endpoint model.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The product object an endpoint belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductKind {
    Environment,
    Sandbox,
    Database,
    Function,
}

impl ProductKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProductKind::Environment => "environment",
            ProductKind::Sandbox => "sandbox",
            ProductKind::Database => "database",
            ProductKind::Function => "function",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProductRef {
    pub kind: ProductKind,
    pub id: i64,
}

impl fmt::Display for ProductRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind.as_str(), self.id)
    }
}

/// Where an endpoint sends traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EndpointTarget {
    /// One revision of a workload. Blue/green changes `revision`.
    Workload {
        workload_id: i64,
        revision: i64,
        port: u16,
    },
    /// A static bundle served by the proxy itself.
    StaticBundle { bundle_id: i64 },
    /// Scale-to-zero: the activator holds the request until an instance of
    /// the workload is `Ready`.
    Activator { workload_id: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostClaimKind {
    /// `<name>.<preview domain>`, minted by Temps.
    Generated,
    /// Under a domain the project owns.
    ProjectDomain,
    /// A customer domain, verified.
    CustomDomain,
    /// A sandbox or branch preview under the preview domain.
    Preview,
}

/// A claim on a hostname. Claims are stored apart from endpoints: who may
/// hold a name is decided once, then routing only reads the winner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostClaim {
    /// Lowercase, no trailing dot. `*.example.test` claims one label.
    pub host: String,
    pub kind: HostClaimKind,
    /// The claim row's id: on two claims for one host, the lower wins.
    pub claim_id: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointProtocol {
    #[default]
    Http,
    /// Routed by SNI, never decrypted.
    TlsPassthrough,
    /// A dedicated port.
    Tcp,
}

/// One entry of an IP rule set: first match wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpRule {
    /// `<address>/<prefix>`.
    pub cidr: String,
    pub allow: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    pub requests: u32,
    pub per_secs: u32,
}

/// Evaluated in order; every policy must pass. Typed, never annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccessPolicy {
    Public,
    /// A password wall; the hash travels in the snapshot by reference.
    Password {
        password_ref: i64,
    },
    /// A Temps user holding `permission` on the owner, proven by a grant
    /// the console signs for this host.
    TempsLogin {
        permission: String,
    },
    SignedLink,
    /// No match: refused when any rule allows, admitted otherwise.
    IpRules {
        rules: Vec<IpRule>,
    },
    RateLimit {
        limit: RateLimit,
    },
    /// ISO 3166-1 alpha-2 codes. An empty `allow` admits every country not
    /// denied.
    Geo {
        #[serde(default)]
        allow: Vec<String>,
        #[serde(default)]
        deny: Vec<String>,
    },
    Captcha,
}

impl AccessPolicy {
    pub fn name(&self) -> &'static str {
        match self {
            AccessPolicy::Public => "public",
            AccessPolicy::Password { .. } => "password",
            AccessPolicy::TempsLogin { .. } => "temps_login",
            AccessPolicy::SignedLink => "signed_link",
            AccessPolicy::IpRules { .. } => "ip_rules",
            AccessPolicy::RateLimit { .. } => "rate_limit",
            AccessPolicy::Geo { .. } => "geo",
            AccessPolicy::Captcha => "captcha",
        }
    }

    /// Needs an HTTP exchange (a redirect, a form, a cookie).
    pub fn needs_http(&self) -> bool {
        matches!(
            self,
            AccessPolicy::Password { .. }
                | AccessPolicy::TempsLogin { .. }
                | AccessPolicy::SignedLink
                | AccessPolicy::Captcha
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: i64,
    pub owner: ProductRef,
    pub target: EndpointTarget,
    pub hosts: Vec<HostClaim>,
    /// ADR-050 path routes. `None` = the whole host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub protocol: EndpointProtocol,
    /// For a `Tcp` endpoint: the server refuses clients that do not
    /// negotiate TLS (PostgreSQL `hostssl`).
    #[serde(default)]
    pub server_requires_tls: bool,
    pub access: Vec<AccessPolicy>,
}
