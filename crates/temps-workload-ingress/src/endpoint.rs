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

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "environment" => Some(ProductKind::Environment),
            "sandbox" => Some(ProductKind::Sandbox),
            "database" => Some(ProductKind::Database),
            "function" => Some(ProductKind::Function),
            _ => None,
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
    /// Whatever the product's existing router resolves today (the route
    /// table's current deployment of an environment, the preview gateway
    /// of a sandbox). Used by endpoints compiled from existing routes until
    /// their product is on the workload plane.
    Product,
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

/// One fixed window: at most `requests` per `per_secs` seconds per client
/// address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    pub requests: u32,
    pub per_secs: u32,
}

/// Which budget a rate limit counts against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitScope {
    /// Each owner has its own budget per client.
    #[default]
    Endpoint,
    /// One budget per client, shared by every endpoint on the instance.
    Instance,
}

impl RateLimitScope {
    fn is_endpoint(&self) -> bool {
        *self == RateLimitScope::Endpoint
    }
}

/// What an IP-rule policy does with a client whose address could not be
/// resolved (a non-INET socket, a garbled forwarded value).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedClient {
    /// Fail closed: an unknown client is refused.
    #[default]
    Deny,
    /// An unknown client passes this policy: a deny list that can only
    /// match an address it knows.
    Admit,
}

impl UnresolvedClient {
    fn is_deny(&self) -> bool {
        *self == UnresolvedClient::Deny
    }
}

/// The kind of an [`AccessPolicy`], without its parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    Public,
    Password,
    TempsLogin,
    SignedLink,
    IpRules,
    RateLimit,
    Geo,
    Captcha,
}

impl PolicyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PolicyKind::Public => "public",
            PolicyKind::Password => "password",
            PolicyKind::TempsLogin => "temps_login",
            PolicyKind::SignedLink => "signed_link",
            PolicyKind::IpRules => "ip_rules",
            PolicyKind::RateLimit => "rate_limit",
            PolicyKind::Geo => "geo",
            PolicyKind::Captcha => "captcha",
        }
    }

    pub(crate) fn bit(&self) -> u16 {
        1 << (*self as u16)
    }
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
    /// First match wins. No match: refused when any rule allows, admitted
    /// otherwise.
    IpRules {
        rules: Vec<IpRule>,
        #[serde(default, skip_serializing_if = "UnresolvedClient::is_deny")]
        unresolved: UnresolvedClient,
    },
    /// Every window must have budget left. A client whose address is
    /// unknown is not counted: there is nothing to attribute it to.
    RateLimit {
        limits: Vec<RateLimit>,
        /// Addresses or CIDRs this limit never counts.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exempt: Vec<String>,
        #[serde(default, skip_serializing_if = "RateLimitScope::is_endpoint")]
        scope: RateLimitScope,
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
    pub fn kind(&self) -> PolicyKind {
        match self {
            AccessPolicy::Public => PolicyKind::Public,
            AccessPolicy::Password { .. } => PolicyKind::Password,
            AccessPolicy::TempsLogin { .. } => PolicyKind::TempsLogin,
            AccessPolicy::SignedLink => PolicyKind::SignedLink,
            AccessPolicy::IpRules { .. } => PolicyKind::IpRules,
            AccessPolicy::RateLimit { .. } => PolicyKind::RateLimit,
            AccessPolicy::Geo { .. } => PolicyKind::Geo,
            AccessPolicy::Captcha => PolicyKind::Captcha,
        }
    }

    pub fn name(&self) -> &'static str {
        self.kind().as_str()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_round_trip_through_json_with_defaults_omitted() {
        let policies = vec![
            AccessPolicy::IpRules {
                rules: vec![IpRule {
                    cidr: "192.0.2.0/24".into(),
                    allow: true,
                }],
                unresolved: UnresolvedClient::Deny,
            },
            AccessPolicy::RateLimit {
                limits: vec![RateLimit {
                    requests: 60,
                    per_secs: 60,
                }],
                exempt: vec![],
                scope: RateLimitScope::Instance,
            },
            AccessPolicy::Password { password_ref: 9 },
        ];
        let json = serde_json::to_string(&policies).unwrap();
        assert_eq!(
            json,
            r#"[{"type":"ip_rules","rules":[{"cidr":"192.0.2.0/24","allow":true}]},{"type":"rate_limit","limits":[{"requests":60,"per_secs":60}],"scope":"instance"},{"type":"password","password_ref":9}]"#
        );
        let back: Vec<AccessPolicy> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, policies);
    }

    #[test]
    fn policy_kinds_have_distinct_bits_and_names() {
        let kinds = [
            PolicyKind::Public,
            PolicyKind::Password,
            PolicyKind::TempsLogin,
            PolicyKind::SignedLink,
            PolicyKind::IpRules,
            PolicyKind::RateLimit,
            PolicyKind::Geo,
            PolicyKind::Captcha,
        ];
        let mut bits = 0u16;
        for kind in kinds {
            assert_eq!(bits & kind.bit(), 0, "{}", kind.as_str());
            bits |= kind.bit();
        }
        assert_eq!(ProductKind::parse("sandbox"), Some(ProductKind::Sandbox));
        assert_eq!(ProductKind::parse("nope"), None);
    }
}
