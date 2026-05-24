//! Shared admin-gate types.
//!
//! The admin gate is a (IP allowlist, host allowlist) pair plus a trust-XFF
//! flag. It governs which clients can reach the management surface (the
//! console dashboard and its APIs) when proxied through the Pingora LB on
//! the public port. The actual enforcement happens in two places:
//!
//! - `temps-proxy` consults the gate during `resolve_peer` so it can decide
//!   whether to fall back to the console for hosts that aren't mapped to a
//!   deployed app.
//! - `temps-cli` mounts an axum middleware on the admin listener for
//!   defense-in-depth when something connects to it directly.
//!
//! Both readers share the same `AdminGateHandle` — an `Arc<ArcSwap<…>>` —
//! so a UI-triggered update reaches every consumer atomically without DB
//! reads on the request path.

use std::net::IpAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Where the active gate configuration came from. Env-supplied configs are
/// frozen at the process level — the UI shows them read-only and refuses to
/// persist DB writes. DB-supplied configs are editable at runtime.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AdminGateSource {
    /// No config present yet (open gate, DB-editable).
    Default,
    /// Loaded from the `settings` row, key `admin_gate`.
    Db,
    /// At least one `TEMPS_ADMIN_*` env var is set; DB writes are rejected.
    Env,
}

#[derive(Debug, Error)]
pub enum AdminGateConfigError {
    #[error("Invalid admin allowlist entry '{raw}': {reason}")]
    InvalidCidr { raw: String, reason: String },
}

/// Snapshot of the active gate configuration. Cheap to clone (shared `Arc`s
/// inside). Treat instances as immutable — mutation happens by swapping the
/// whole struct via [`AdminGateHandle::store`].
#[derive(Clone, Debug)]
pub struct AdminGateConfig {
    /// Allowed source networks. Empty = allow any source.
    pub allowed_nets: Arc<Vec<IpNet>>,
    /// Allowed `Host` header values (port stripped, lowercased).
    /// Empty = allow any host.
    pub allowed_hosts: Arc<Vec<String>>,
    /// When true, the gate honors an `X-Forwarded-For` header — but only
    /// when the immediate peer is loopback, so an external client cannot
    /// spoof their source IP by setting the header themselves.
    pub trust_forwarded_for: bool,
    /// Where this snapshot came from. Surfaced via the management API so
    /// the UI can render an "env override" banner.
    pub source: AdminGateSource,
}

impl AdminGateConfig {
    /// Build a config from the parsed env vars. `source` is `Env` when any
    /// of the inputs are non-default, `Default` otherwise.
    pub fn from_env(
        allowed_ips: &[String],
        allowed_hosts: &[String],
        trust_forwarded_for: bool,
    ) -> Result<Self, AdminGateConfigError> {
        let env_active =
            !allowed_ips.is_empty() || !allowed_hosts.is_empty() || trust_forwarded_for;
        let source = if env_active {
            AdminGateSource::Env
        } else {
            AdminGateSource::Default
        };
        Self::from_parts(allowed_ips, allowed_hosts, trust_forwarded_for, source)
    }

    /// Build a config from raw inputs and an explicit source. Used by the
    /// management service when persisting UI-supplied values.
    pub fn from_parts(
        allowed_ips: &[String],
        allowed_hosts: &[String],
        trust_forwarded_for: bool,
        source: AdminGateSource,
    ) -> Result<Self, AdminGateConfigError> {
        let allowed_nets = allowed_ips
            .iter()
            .map(|raw| parse_cidr(raw))
            .collect::<Result<Vec<_>, _>>()?;

        let allowed_hosts = allowed_hosts
            .iter()
            .map(|h| h.trim().to_lowercase())
            .filter(|h| !h.is_empty())
            .collect::<Vec<_>>();

        Ok(Self {
            allowed_nets: Arc::new(allowed_nets),
            allowed_hosts: Arc::new(allowed_hosts),
            trust_forwarded_for,
            source,
        })
    }

    /// Returns true when no gate is configured (both allowlists empty).
    /// Callers may skip enforcement entirely in that case.
    pub fn is_noop(&self) -> bool {
        self.allowed_nets.is_empty() && self.allowed_hosts.is_empty()
    }

    /// Returns true when this config can be written through the management
    /// API. Env-supplied configs are read-only.
    pub fn is_editable(&self) -> bool {
        !matches!(self.source, AdminGateSource::Env)
    }

    /// Evaluate the gate against a candidate (ip, host) tuple. Used by the
    /// lockout pre-flight in the management handler so we never persist a
    /// config that would deny the admin saving it, and by the proxy when
    /// deciding whether to forward to the console fallback.
    pub fn would_allow(&self, ip: IpAddr, host: Option<&str>) -> bool {
        if !ip_matches(ip, &self.allowed_nets) {
            return false;
        }
        if self.allowed_hosts.is_empty() {
            return true;
        }
        match host {
            Some(h) => {
                let h = h.split(':').next().unwrap_or(h).to_lowercase();
                self.allowed_hosts.iter().any(|allowed| allowed == &h)
            }
            None => false,
        }
    }
}

/// Shared, atomically-swappable handle around the active config. Readers
/// clone the handle and call [`current`](Self::current) per request — one
/// atomic load, no locks, no DB hits.
#[derive(Clone)]
pub struct AdminGateHandle(Arc<ArcSwap<AdminGateConfig>>);

impl AdminGateHandle {
    pub fn new(initial: AdminGateConfig) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(initial)))
    }

    /// Snapshot the current config. Cheap — clones an Arc.
    pub fn current(&self) -> Arc<AdminGateConfig> {
        self.0.load_full()
    }

    /// Atomically swap to a new config. Returns the previous one.
    pub fn store(&self, next: AdminGateConfig) -> Arc<AdminGateConfig> {
        self.0.swap(Arc::new(next))
    }
}

impl std::fmt::Debug for AdminGateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminGateHandle")
            .field("current", &*self.current())
            .finish()
    }
}

/// Parse a single allowlist entry. Bare IPs are upgraded to /32 (v4) or
/// /128 (v6) so the rest of the code can treat everything as a network.
pub fn parse_cidr(raw: &str) -> Result<IpNet, AdminGateConfigError> {
    let trimmed = raw.trim();
    if let Ok(net) = trimmed.parse::<IpNet>() {
        return Ok(net);
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        let net = match ip {
            IpAddr::V4(v4) => IpNet::V4(ipnet::Ipv4Net::new(v4, 32).unwrap()),
            IpAddr::V6(v6) => IpNet::V6(ipnet::Ipv6Net::new(v6, 128).unwrap()),
        };
        return Ok(net);
    }
    Err(AdminGateConfigError::InvalidCidr {
        raw: trimmed.to_string(),
        reason: "expected an IP address or CIDR (e.g. 10.0.0.0/8)".into(),
    })
}

/// Returns true when `ip` falls inside any network in `allowed`. An empty
/// list means "any IP is allowed".
pub fn ip_matches(ip: IpAddr, allowed: &[IpNet]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    allowed.iter().any(|net| net.contains(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn bare_ip_is_upgraded_to_host_route() {
        let net = parse_cidr("10.0.0.1").unwrap();
        assert!(net.contains(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!net.contains(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))));
    }

    #[test]
    fn cidr_is_parsed() {
        let net = parse_cidr("10.0.0.0/8").unwrap();
        assert!(net.contains(&IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
        assert!(!net.contains(&IpAddr::V4(Ipv4Addr::new(11, 0, 0, 0))));
    }

    #[test]
    fn ipv6_cidr_is_parsed() {
        let net = parse_cidr("2001:db8::/32").unwrap();
        assert!(net.contains(&IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap())));
        assert!(!net.contains(&IpAddr::V6("2001:dead::1".parse::<Ipv6Addr>().unwrap())));
    }

    #[test]
    fn from_env_marks_source_env_when_any_field_set() {
        let c = AdminGateConfig::from_env(&["10.0.0.0/8".into()], &[], false).unwrap();
        assert_eq!(c.source, AdminGateSource::Env);
        let c = AdminGateConfig::from_env(&[], &["x".into()], false).unwrap();
        assert_eq!(c.source, AdminGateSource::Env);
        let c = AdminGateConfig::from_env(&[], &[], true).unwrap();
        assert_eq!(c.source, AdminGateSource::Env);
    }

    #[test]
    fn from_env_marks_source_default_when_all_empty() {
        let c = AdminGateConfig::from_env(&[], &[], false).unwrap();
        assert_eq!(c.source, AdminGateSource::Default);
        assert!(c.is_editable());
        assert!(c.is_noop());
    }

    #[test]
    fn handle_swap_is_observable() {
        let initial = AdminGateConfig::from_env(&[], &[], false).unwrap();
        let handle = AdminGateHandle::new(initial);
        assert_eq!(handle.current().source, AdminGateSource::Default);

        let next =
            AdminGateConfig::from_parts(&["10.0.0.0/8".into()], &[], false, AdminGateSource::Db)
                .unwrap();
        handle.store(next);
        assert_eq!(handle.current().source, AdminGateSource::Db);
        assert_eq!(handle.current().allowed_nets.len(), 1);
    }

    #[test]
    fn would_allow_matches_ip_and_host() {
        let c = AdminGateConfig::from_parts(
            &["10.0.0.0/8".into()],
            &["admin.example.com".into()],
            false,
            AdminGateSource::Db,
        )
        .unwrap();
        let ip_in = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
        let ip_out = IpAddr::V4(Ipv4Addr::new(11, 0, 0, 0));

        assert!(c.would_allow(ip_in, Some("admin.example.com")));
        assert!(c.would_allow(ip_in, Some("Admin.Example.COM:443")));
        assert!(!c.would_allow(ip_out, Some("admin.example.com")));
        assert!(!c.would_allow(ip_in, Some("evil.example.com")));
        assert!(!c.would_allow(ip_in, None));
    }

    #[test]
    fn would_allow_with_empty_host_list_ignores_host() {
        let c =
            AdminGateConfig::from_parts(&["10.0.0.0/8".into()], &[], false, AdminGateSource::Db)
                .unwrap();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
        assert!(c.would_allow(ip, None));
        assert!(c.would_allow(ip, Some("anything.example")));
    }
}
