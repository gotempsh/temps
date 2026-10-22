// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A [`WorkloadPolicy`] as a microsandbox SDK network policy.
//!
//! A microVM has no bridge the host's nftables can see: its traffic leaves
//! through the SDK's user-space network stack, which enforces its own
//! policy. These types serialize to exactly the JSON the SDK's
//! `NetworkPolicy` reads (`temps-agents` checks that against the pinned
//! SDK), so this crate does not depend on the SDK itself.
//!
//! The translation keeps the nftables semantics:
//! - denies first, then allows, then the profile's default;
//! - a workload peer becomes one rule per current address;
//! - a `none` profile drops its allow rules, an `open` profile ignores
//!   them (both reported);
//! - the host is always allowed in, ahead of every ingress deny, as it is
//!   under nftables (the host's own traffic never crosses `forward`, so
//!   the proxy and health checks reach the workload whatever its rules).
//!   The SDK sees the host twice: as the gateway addresses (`host`) and,
//!   on a published port, as the host socket's own peer, which is loopback
//!   for the default loopback binding (`loopback`). Both are allowed;
//! - restricted egress keeps DNS to the sandbox gateway. A container
//!   resolves through Docker's resolver inside its own namespace, which
//!   the `forward` hook never sees, so a linked peer stays reachable by
//!   name; a microVM's resolver *is* the gateway, and without this rule
//!   restricted egress would silently break name resolution. It follows
//!   the denies, so an operator deny of the host on port 53 still wins.
//!   A `none` profile gets no DNS, as with the SDK's empty profile set.

use serde::{Deserialize, Serialize};

use crate::identity::IdentityMap;
use crate::rule::{Action, Direction, L4Protocol, Peer, Profile, Rule, WorkloadPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkDirection {
    Egress,
    Ingress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkDestination {
    Cidr(String),
    /// A pre-defined SDK group; only `host` and `loopback` are produced.
    Group(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkProtocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkPortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkRule {
    pub direction: SdkDirection,
    pub destination: SdkDestination,
    pub protocols: Vec<SdkProtocol>,
    pub ports: Vec<SdkPortRange>,
    pub action: SdkAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkNetworkPolicy {
    pub default_egress: SdkAction,
    pub default_ingress: SdkAction,
    pub rules: Vec<SdkRule>,
}

/// The translated policy and what could not be expressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    pub policy: SdkNetworkPolicy,
    pub warnings: Vec<String>,
}

fn default_action(profile: Profile) -> SdkAction {
    match profile {
        Profile::Open => SdkAction::Allow,
        Profile::Restricted | Profile::None => SdkAction::Deny,
    }
}

pub fn to_sdk_policy(policy: &WorkloadPolicy, identities: &IdentityMap) -> Translated {
    let mut warnings = Vec::new();
    let mut rules = Vec::new();
    for direction in [Direction::Egress, Direction::Ingress] {
        let profile = policy.profile(direction);
        let mut selected: Vec<&Rule> = policy
            .rules
            .iter()
            .filter(|r| r.direction == direction)
            .collect();
        selected.sort_by_key(|r| (r.action == Action::Allow, r.id));
        let mut dns_pending = direction == Direction::Egress && profile == Profile::Restricted;
        // The host first, ahead of any deny: under nftables its traffic
        // never crosses `forward`, so no rule can shut the proxy out.
        if direction == Direction::Ingress && (profile != Profile::Open || !selected.is_empty()) {
            rules.extend(host_ingress());
        }
        for rule in selected {
            if dns_pending && rule.action == Action::Allow {
                rules.push(gateway_dns());
                dns_pending = false;
            }
            let action = match (rule.action, profile) {
                (Action::Deny, _) => SdkAction::Deny,
                (Action::Allow, Profile::Restricted) => SdkAction::Allow,
                (Action::Allow, Profile::None | Profile::Open) => {
                    warnings.push(format!(
                        "rule {} of {} is an allow under a '{}' profile and has no effect",
                        rule.id,
                        policy.subject,
                        if profile == Profile::None {
                            "none"
                        } else {
                            "open"
                        }
                    ));
                    continue;
                }
            };
            let destinations: Vec<String> = match rule.peer {
                Peer::Cidr(cidr) => vec![cidr.to_string()],
                Peer::Workload(peer) => identities
                    .addresses(&peer)
                    .map(|a| crate::rule::Cidr::host(a).to_string())
                    .collect(),
            };
            for destination in destinations {
                rules.push(SdkRule {
                    direction: sdk_direction(direction),
                    destination: SdkDestination::Cidr(destination),
                    protocols: protocols(rule),
                    ports: rule
                        .ports
                        .map(|p| {
                            vec![SdkPortRange {
                                start: p.start,
                                end: p.end,
                            }]
                        })
                        .unwrap_or_default(),
                    action,
                });
            }
        }
        if dns_pending {
            rules.push(gateway_dns());
        }
    }
    Translated {
        policy: SdkNetworkPolicy {
            default_egress: default_action(policy.egress),
            default_ingress: default_action(policy.ingress),
            rules,
        },
        warnings,
    }
}

/// The host's ways in: through the gateway, and to a published port, whose
/// listener sees the host-side peer (loopback for a loopback binding).
fn host_ingress() -> [SdkRule; 2] {
    ["host", "loopback"].map(|group| SdkRule {
        direction: SdkDirection::Ingress,
        destination: SdkDestination::Group(group.to_string()),
        protocols: vec![],
        ports: vec![],
        action: SdkAction::Allow,
    })
}

/// Plain DNS to the sandbox gateway; the SDK's own `Rule::allow_dns`.
fn gateway_dns() -> SdkRule {
    SdkRule {
        direction: SdkDirection::Egress,
        destination: SdkDestination::Group("host".to_string()),
        protocols: vec![SdkProtocol::Udp, SdkProtocol::Tcp],
        ports: vec![SdkPortRange { start: 53, end: 53 }],
        action: SdkAction::Allow,
    }
}

fn sdk_direction(direction: Direction) -> SdkDirection {
    match direction {
        Direction::Egress => SdkDirection::Egress,
        Direction::Ingress => SdkDirection::Ingress,
    }
}

fn protocols(rule: &Rule) -> Vec<SdkProtocol> {
    match (rule.protocol, rule.ports.is_some()) {
        (L4Protocol::Tcp, _) => vec![SdkProtocol::Tcp],
        (L4Protocol::Udp, _) => vec![SdkProtocol::Udp],
        (L4Protocol::Any, true) => vec![SdkProtocol::Tcp, SdkProtocol::Udp],
        (L4Protocol::Any, false) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Origin, PortRange};
    use temps_workload::{WorkloadKind, WorkloadRef};

    fn subject() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, 1)
    }

    fn db() -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Service, 9)
    }

    fn policy() -> WorkloadPolicy {
        WorkloadPolicy {
            subject: subject(),
            egress: Profile::Restricted,
            ingress: Profile::Restricted,
            rules: vec![
                Rule {
                    id: 2,
                    direction: Direction::Egress,
                    peer: Peer::Workload(db()),
                    ports: Some(PortRange::single(5432)),
                    protocol: L4Protocol::Tcp,
                    action: Action::Allow,
                    origin: Origin::Link,
                },
                Rule {
                    id: 3,
                    direction: Direction::Egress,
                    peer: Peer::Cidr("10.0.0.0/8".parse().unwrap()),
                    ports: None,
                    protocol: L4Protocol::Any,
                    action: Action::Deny,
                    origin: Origin::Operator,
                },
            ],
        }
    }

    #[test]
    fn restricted_policy_translates_deny_first_with_per_address_rules() {
        let ids = IdentityMap::new()
            .with(db(), "172.20.0.9".parse().unwrap())
            .with(db(), "fd00::9".parse().unwrap());
        let out = to_sdk_policy(&policy(), &ids);
        let json = serde_json::to_value(&out.policy).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "default_egress": "deny",
                "default_ingress": "deny",
                "rules": [
                    {"direction": "egress", "destination": {"cidr": "10.0.0.0/8"}, "protocols": [], "ports": [], "action": "deny"},
                    {"direction": "egress", "destination": {"group": "host"}, "protocols": ["udp", "tcp"], "ports": [{"start": 53, "end": 53}], "action": "allow"},
                    {"direction": "egress", "destination": {"cidr": "172.20.0.9/32"}, "protocols": ["tcp"], "ports": [{"start": 5432, "end": 5432}], "action": "allow"},
                    {"direction": "egress", "destination": {"cidr": "fd00::9/128"}, "protocols": ["tcp"], "ports": [{"start": 5432, "end": 5432}], "action": "allow"},
                    {"direction": "ingress", "destination": {"group": "host"}, "protocols": [], "ports": [], "action": "allow"},
                    {"direction": "ingress", "destination": {"group": "loopback"}, "protocols": [], "ports": [], "action": "allow"}
                ]
            })
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn a_peer_with_no_address_matches_nothing() {
        let out = to_sdk_policy(&policy(), &IdentityMap::new());
        // The deny, gateway DNS, and the host's two ways in.
        assert_eq!(out.policy.rules.len(), 4, "{:?}", out.policy.rules);
        assert_eq!(out.policy.default_egress, SdkAction::Deny);
    }

    #[test]
    fn open_profiles_default_to_allow_and_none_drops_allows() {
        let mut p = policy();
        p.ingress = Profile::Open;
        p.egress = Profile::None;
        let out = to_sdk_policy(&p, &IdentityMap::new());
        assert_eq!(out.policy.default_ingress, SdkAction::Allow);
        assert_eq!(out.policy.default_egress, SdkAction::Deny);
        // Open ingress with no ingress rule needs no host rule either.
        assert!(out.policy.rules.iter().all(|r| r.action == SdkAction::Deny));
        assert_eq!(out.warnings.len(), 1);
    }

    #[test]
    fn an_ingress_deny_never_shuts_the_host_out() {
        let mut p = policy();
        p.rules = vec![Rule {
            id: 4,
            direction: Direction::Ingress,
            peer: Peer::Cidr("0.0.0.0/0".parse().unwrap()),
            ports: None,
            protocol: L4Protocol::Any,
            action: Action::Deny,
            origin: Origin::Operator,
        }];
        p.ingress = Profile::Open;
        let out = to_sdk_policy(&p, &IdentityMap::new());
        let ingress: Vec<_> = out
            .policy
            .rules
            .iter()
            .filter(|r| r.direction == SdkDirection::Ingress)
            .collect();
        assert_eq!(ingress.len(), 3);
        assert_eq!(
            ingress[0].destination,
            SdkDestination::Group("host".to_string())
        );
        assert_eq!(
            ingress[1].destination,
            SdkDestination::Group("loopback".to_string())
        );
        assert!(ingress[..2].iter().all(|r| r.action == SdkAction::Allow));
        assert_eq!(ingress[2].action, SdkAction::Deny);
    }

    #[test]
    fn restricted_egress_without_rules_still_resolves_names() {
        let mut p = policy();
        p.rules.clear();
        p.ingress = Profile::Open;
        let out = to_sdk_policy(&p, &IdentityMap::new());
        assert_eq!(out.policy.rules, vec![gateway_dns()]);
        p.egress = Profile::None;
        assert!(to_sdk_policy(&p, &IdentityMap::new())
            .policy
            .rules
            .is_empty());
    }
}
