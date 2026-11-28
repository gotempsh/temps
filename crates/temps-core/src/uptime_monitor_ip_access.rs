// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Whether a project's own IP access policy refuses Temps' managed uptime
//! monitor.
//!
//! A managed environment monitor does not dial the public internet: it
//! connects to this server's proxy listener and asks for the app's hostname
//! (see `temps-status-page`'s `local_proxy_client`). The proxy then applies
//! the project's IP access policy to that connection like any other, so an
//! allow list that names only operators and a tailnet answers the monitor
//! with the same generic 403 every outsider gets, and the monitor reports
//! the app as degraded forever.
//!
//! The proxy must keep that 403 generic. This module lets the monitor side
//! explain it instead:
//!
//! * [`monitor_source_addresses`] says which addresses the monitor's
//!   connections come from, from the configured listeners;
//! * [`UptimeMonitorIpAccess`] is implemented by the crate that owns the
//!   policy (`temps-proxy`), evaluating the stored policy with the matcher
//!   the proxy enforces it with;
//! * [`UptimeMonitorIpBlock`] is the typed answer the monitor status and the
//!   IP access settings both return, so the console and the CLI can offer
//!   the fix.
//!
//! Nothing here runs on the proxy's request path.

use std::net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

use crate::plugin::ServiceRegistrationContext;

/// The description the suggested allow-list entries carry.
pub const MONITOR_ENTRY_DESCRIPTION: &str = "Temps uptime monitor";

/// How a scope's CIDR list is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum IpAccessMode {
    /// Entries are kept but nothing is enforced.
    Disabled,
    /// Only clients inside a listed CIDR may reach the scope.
    Allow,
    /// Clients inside a listed CIDR are blocked; everyone else is allowed.
    Deny,
}

impl IpAccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IpAccessMode::Disabled => "disabled",
            IpAccessMode::Allow => "allow",
            IpAccessMode::Deny => "deny",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "disabled" => Some(IpAccessMode::Disabled),
            "allow" => Some(IpAccessMode::Allow),
            "deny" => Some(IpAccessMode::Deny),
            _ => None,
        }
    }
}

/// Turn a proxy bind address into the address a local client connects to.
///
/// A wildcard (`0.0.0.0`, `::`) describes where a listener binds, not a
/// destination, so it becomes the loopback address of the same family. A
/// specific address is kept as it is.
pub fn local_probe_destination(listener: &str) -> Result<SocketAddr, AddrParseError> {
    let address = listener.parse::<SocketAddr>()?;
    let ip = match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Ok(SocketAddr::new(ip, address.port()))
}

/// Fold an IPv4-mapped IPv6 address into IPv4, as the proxy does before it
/// matches a client address.
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        ip => ip,
    }
}

/// The address the proxy sees a monitor connection come from, for one
/// listener.
///
/// The monitor connects to [`local_probe_destination`]. That is an address
/// of this host, and a connection to one of the host's own addresses leaves
/// from that same address: a wildcard listener sees loopback, a listener
/// bound to the server's public address sees the public address. This is
/// why "allow 127.0.0.1" is not a fix on its own.
pub fn monitor_source_address(listener: &str) -> Option<IpAddr> {
    local_probe_destination(listener)
        .ok()
        .map(|destination| normalize(destination.ip()))
}

/// Every address the managed monitor may connect from on this server: the
/// HTTP listener's, then the TLS listener's (HTTPS monitors, and the HTTPS
/// follow-up of a forced redirect), without duplicates. A listener that does
/// not parse contributes nothing: the monitor cannot use it either.
pub fn monitor_source_addresses(http_listener: &str, tls_listener: Option<&str>) -> Vec<IpAddr> {
    let mut addresses = Vec::with_capacity(2);
    for listener in std::iter::once(http_listener).chain(tls_listener) {
        if let Some(ip) = monitor_source_address(listener) {
            if !addresses.contains(&ip) {
                addresses.push(ip);
            }
        }
    }
    addresses
}

/// A single-host CIDR for `ip`.
pub fn host_cidr(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => format!("{ip}/32"),
        IpAddr::V6(ip) => format!("{ip}/128"),
    }
}

/// Where the policy that applies is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum IpAccessPolicyScope {
    /// The project-wide list (inherited by every environment without its own).
    Project,
    /// The environment's own list, which replaces the project-wide one.
    Environment,
}

impl IpAccessPolicyScope {
    fn noun(self) -> &'static str {
        match self {
            IpAccessPolicyScope::Project => "project",
            IpAccessPolicyScope::Environment => "environment",
        }
    }
}

/// Why the policy refuses the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UptimeMonitorBlockReason {
    /// An allow list that does not include the monitor's address.
    NotAllowListed,
    /// A deny list with an entry that covers the monitor's address.
    DenyListed,
    /// The saved policy cannot be read, so the proxy refuses every address.
    Unreadable,
}

/// A saved IP access policy refuses Temps' managed uptime monitor, so the
/// monitor sees a 403 instead of the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UptimeMonitorIpBlock {
    pub project_id: i32,
    /// The environment the evaluation was for; `null` when the project-wide
    /// list was evaluated on its own.
    pub environment_id: Option<i32>,
    /// Which list refuses the monitor. With `environment`, the fix belongs
    /// on `environment_id`'s own list; with `project`, on the project list.
    pub policy_scope: IpAccessPolicyScope,
    pub mode: IpAccessMode,
    pub reason: UptimeMonitorBlockReason,
    /// Every address Temps' uptime monitor connects from on this server.
    #[schema(example = json!(["127.0.0.1", "203.0.113.10"]))]
    pub monitor_addresses: Vec<String>,
    /// The monitor addresses the policy refuses.
    #[schema(example = json!(["203.0.113.10"]))]
    pub blocked_addresses: Vec<String>,
    /// For a deny list: the entries that cover a monitor address.
    pub matched_cidrs: Vec<String>,
    /// For an allow list: the entries that, added to it, let the monitor
    /// through. Empty when the fix is to remove entries or save the list.
    #[schema(example = json!(["203.0.113.10/32"]))]
    pub suggested_cidrs: Vec<String>,
    /// The description to give the suggested entries.
    #[schema(example = "Temps uptime monitor")]
    pub suggested_description: String,
    /// One-paragraph explanation suitable for display and for a check result.
    pub message: String,
}

fn join(values: &[String]) -> String {
    match values {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

impl UptimeMonitorIpBlock {
    /// Build the block, and its message, from what the evaluation found.
    ///
    /// `blocked` must be non-empty; `matched_cidrs` matters for
    /// [`UptimeMonitorBlockReason::DenyListed`] only.
    #[allow(clippy::too_many_arguments, reason = "plain data constructor")]
    pub fn new(
        project_id: i32,
        environment_id: Option<i32>,
        policy_scope: IpAccessPolicyScope,
        mode: IpAccessMode,
        reason: UptimeMonitorBlockReason,
        monitor_addresses: &[IpAddr],
        blocked: &[IpAddr],
        matched_cidrs: Vec<String>,
    ) -> Self {
        let blocked_addresses: Vec<String> = blocked.iter().map(IpAddr::to_string).collect();
        let suggested_cidrs: Vec<String> = match reason {
            UptimeMonitorBlockReason::NotAllowListed => {
                blocked.iter().copied().map(host_cidr).collect()
            }
            UptimeMonitorBlockReason::DenyListed | UptimeMonitorBlockReason::Unreadable => {
                Vec::new()
            }
        };
        let list = format!("this {}'s IP access list", policy_scope.noun());
        let from = join(&blocked_addresses);
        // Only the probes from the blocked addresses are refused: with HTTP
        // on loopback and HTTPS on a public address, one can pass while the
        // other is answered with 403.
        let effect = format!(
            "so the monitor's checks from {} are answered with HTTP 403 and the app's health is \
             unverified",
            if blocked.len() == 1 {
                "that address"
            } else {
                "those addresses"
            }
        );
        let message = match reason {
            UptimeMonitorBlockReason::NotAllowListed => format!(
                "Blocked by {list}: Temps' uptime monitor connects from {from}, which the list \
                 does not allow, {effect}. Add {} to the list to let the monitor through.",
                join(&suggested_cidrs)
            ),
            UptimeMonitorBlockReason::DenyListed => format!(
                "Blocked by {list}: Temps' uptime monitor connects from {from}, which the deny \
                 list blocks ({}), {effect}. Remove {} from the list to let the monitor through.",
                join(&matched_cidrs),
                if matched_cidrs.len() == 1 {
                    "that entry"
                } else {
                    "those entries"
                }
            ),
            UptimeMonitorBlockReason::Unreadable => format!(
                "Blocked by {list}: the saved list cannot be read, so it refuses every address, \
                 including Temps' uptime monitor at {from}, so every check is answered with HTTP \
                 403 and the app's health is unverified. Save the list again."
            ),
        };
        Self {
            project_id,
            environment_id,
            policy_scope,
            mode,
            reason,
            monitor_addresses: monitor_addresses.iter().map(IpAddr::to_string).collect(),
            blocked_addresses,
            matched_cidrs,
            suggested_cidrs,
            suggested_description: MONITOR_ENTRY_DESCRIPTION.to_string(),
            message,
        }
    }
}

#[derive(Debug, Error)]
pub enum UptimeMonitorIpAccessError {
    #[error(
        "Could not read the IP access policy of environment {environment_id} in project \
         {project_id}: {reason}"
    )]
    PolicyUnavailable {
        project_id: i32,
        environment_id: i32,
        reason: String,
    },
}

/// Evaluates the stored IP access policy against the monitor's addresses.
#[async_trait]
pub trait UptimeMonitorIpAccess: Send + Sync {
    /// The block on `environment_id`'s monitor from the policy that governs
    /// it (its own list, else the project's), or `None` when every address
    /// in `monitor_addresses` gets through.
    async fn monitor_block(
        &self,
        project_id: i32,
        environment_id: i32,
        monitor_addresses: &[IpAddr],
    ) -> Result<Option<UptimeMonitorIpBlock>, UptimeMonitorIpAccessError>;
}

/// Where the policy owner puts its [`UptimeMonitorIpAccess`]. The status
/// page registers before the proxy plugin, so it holds this slot and reads
/// it at check time. Empty in a binary without the proxy plugin, where the
/// monitor keeps reporting the bare HTTP status.
#[derive(Default)]
pub struct UptimeMonitorIpAccessSlot {
    inner: OnceLock<Arc<dyn UptimeMonitorIpAccess>>,
}

impl UptimeMonitorIpAccessSlot {
    /// The shared slot in `context`, registering an empty one when no plugin
    /// has yet.
    pub fn get_or_register(context: &ServiceRegistrationContext) -> Arc<Self> {
        if let Some(slot) = context.get_service::<Self>() {
            return slot;
        }
        let slot = Arc::new(Self::default());
        context.register_service(slot.clone());
        slot
    }

    /// Fill the slot. A second call is ignored: the first implementation
    /// stays.
    pub fn set(&self, access: Arc<dyn UptimeMonitorIpAccess>) -> bool {
        self.inner.set(access).is_ok()
    }

    pub fn get(&self) -> Option<Arc<dyn UptimeMonitorIpAccess>> {
        self.inner.get().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test IP is valid")
    }

    #[test]
    fn mode_round_trips() {
        for mode in [
            IpAccessMode::Disabled,
            IpAccessMode::Allow,
            IpAccessMode::Deny,
        ] {
            assert_eq!(IpAccessMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(IpAccessMode::parse("block"), None);
    }

    #[test]
    fn wildcard_listeners_are_reached_from_loopback_of_the_same_family() {
        assert_eq!(monitor_source_address("0.0.0.0:80"), Some(ip("127.0.0.1")));
        assert_eq!(monitor_source_address("[::]:443"), Some(ip("::1")));
        assert_eq!(
            local_probe_destination("0.0.0.0:8080").expect("valid listener"),
            "127.0.0.1:8080".parse().expect("valid address")
        );
    }

    #[test]
    fn a_specific_listener_is_reached_from_that_address() {
        assert_eq!(
            monitor_source_address("203.0.113.10:443"),
            Some(ip("203.0.113.10"))
        );
        assert_eq!(
            monitor_source_address("[2001:db8::10]:443"),
            Some(ip("2001:db8::10"))
        );
        assert_eq!(
            monitor_source_address("127.0.0.1:8080"),
            Some(ip("127.0.0.1"))
        );
        // An IPv4-mapped listener is matched as IPv4, as the proxy does.
        assert_eq!(
            monitor_source_address("[::ffff:203.0.113.10]:443"),
            Some(ip("203.0.113.10"))
        );
    }

    #[test]
    fn source_addresses_cover_both_listeners_without_duplicates() {
        // The reported case: plain HTTP on every address, HTTPS bound to the
        // server's public address. Both are sources.
        assert_eq!(
            monitor_source_addresses("0.0.0.0:80", Some("203.0.113.10:443")),
            vec![ip("127.0.0.1"), ip("203.0.113.10")]
        );
        assert_eq!(
            monitor_source_addresses("0.0.0.0:80", Some("0.0.0.0:443")),
            vec![ip("127.0.0.1")]
        );
        assert_eq!(
            monitor_source_addresses("[::]:80", Some("0.0.0.0:443")),
            vec![ip("::1"), ip("127.0.0.1")]
        );
        assert_eq!(
            monitor_source_addresses("0.0.0.0:80", None),
            vec![ip("127.0.0.1")]
        );
        // A listener that does not parse contributes nothing.
        assert_eq!(
            monitor_source_addresses("not-an-address", Some("203.0.113.10:443")),
            vec![ip("203.0.113.10")]
        );
    }

    #[test]
    fn host_cidrs_use_the_family_prefix() {
        assert_eq!(host_cidr(ip("203.0.113.10")), "203.0.113.10/32");
        assert_eq!(host_cidr(ip("::1")), "::1/128");
    }

    #[test]
    fn an_allow_list_block_names_the_address_and_the_entry_to_add() {
        let block = UptimeMonitorIpBlock::new(
            4,
            Some(9),
            IpAccessPolicyScope::Project,
            IpAccessMode::Allow,
            UptimeMonitorBlockReason::NotAllowListed,
            &[ip("127.0.0.1"), ip("203.0.113.10")],
            &[ip("203.0.113.10")],
            Vec::new(),
        );
        assert_eq!(block.suggested_cidrs, vec!["203.0.113.10/32"]);
        assert_eq!(block.suggested_description, MONITOR_ENTRY_DESCRIPTION);
        assert_eq!(block.monitor_addresses, vec!["127.0.0.1", "203.0.113.10"]);
        assert!(
            block.message.starts_with(
                "Blocked by this project's IP access list: Temps' uptime monitor connects \
                 from 203.0.113.10, which the list does not allow"
            ),
            "{}",
            block.message
        );
        assert!(block.message.contains("Add 203.0.113.10/32"));
        // Loopback is admitted, so only the probes from the public address
        // are refused, not every check.
        assert!(
            block
                .message
                .contains("the monitor's checks from that address are answered with HTTP 403"),
            "{}",
            block.message
        );
        assert!(!block.message.contains("every check"), "{}", block.message);
    }

    #[test]
    fn a_deny_list_block_suggests_removing_rather_than_adding() {
        let block = UptimeMonitorIpBlock::new(
            4,
            Some(9),
            IpAccessPolicyScope::Environment,
            IpAccessMode::Deny,
            UptimeMonitorBlockReason::DenyListed,
            &[ip("127.0.0.1")],
            &[ip("127.0.0.1")],
            vec!["127.0.0.0/8".to_string()],
        );
        assert!(block.suggested_cidrs.is_empty());
        assert!(block.message.contains("this environment's IP access list"));
        assert!(block.message.contains("(127.0.0.0/8)"));
        assert!(block.message.contains("Remove that entry"));
    }

    #[test]
    fn several_addresses_are_listed_in_prose() {
        assert_eq!(join(&["a".into()]), "a");
        assert_eq!(join(&["a".into(), "b".into()]), "a and b");
        assert_eq!(join(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    #[test]
    fn the_slot_is_write_once() {
        struct Never;
        #[async_trait]
        impl UptimeMonitorIpAccess for Never {
            async fn monitor_block(
                &self,
                _project_id: i32,
                _environment_id: i32,
                _monitor_addresses: &[IpAddr],
            ) -> Result<Option<UptimeMonitorIpBlock>, UptimeMonitorIpAccessError> {
                Ok(None)
            }
        }
        let slot = UptimeMonitorIpAccessSlot::default();
        assert!(slot.get().is_none());
        assert!(slot.set(Arc::new(Never)));
        assert!(!slot.set(Arc::new(Never)));
        assert!(slot.get().is_some());
    }
}
