// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Policy-aware internal DNS (ADR-055 §10, ADR-052 §4).
//!
//! The node resolver identifies who is asking by source address and asks
//! [`DnsPolicy::decide`] what to do:
//!
//! - **An unrestricted source** (open workloads, the host, anything not in
//!   the identity map) gets today's behaviour: the `*.temps.local` zone,
//!   and everything else forwarded upstream.
//! - **A restricted source** only resolves the names its egress rules
//!   allow: names owned by a workload it may reach, its own names, and the
//!   group short names of those. Every other name, inside the zone or not,
//!   is NXDOMAIN, and nothing is forwarded: external names are resolved
//!   only by the egress proxy, which checks the allow-list and the
//!   public-address rule. Forwarding them would let a sandbox leak data
//!   through DNS queries past the proxy.
//!
//! **Group short names.** Each project group gets a zone,
//! `<group>.grp.temps.local`, set as the members' DNS search domain, so a
//! member resolves `db` as `db.<group>.grp.temps.local`. A short name is an
//! alias of the member's canonical name and follows policy like any other.
//!
//! The policy is an immutable snapshot built off the query path and
//! published behind an `ArcSwap`; a decision is two hash lookups and never
//! allocates.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use temps_workload::WorkloadRef;

use crate::identity::IdentityMap;
use crate::rule::{Action, Direction, Peer, Profile, WorkloadPolicy};

/// The internal zone every node serves.
pub const ZONE: &str = "temps.local";
/// The resolver's own marker name (`temps-node-dns`), which every
/// source may query: it is how a node checks its resolver is the one in
/// front of a workload.
pub const MARKER: &str = "_temps-resolver.temps.local";

/// The search domain of a project group's members.
pub fn group_zone(group: &str) -> String {
    format!("{}.grp.{ZONE}", group.to_ascii_lowercase())
}

/// A member's short name inside its group's zone.
pub fn group_short_name(member: &str, group: &str) -> String {
    format!("{}.{}", member.to_ascii_lowercase(), group_zone(group))
}

/// `name` lower-cased, without the trailing dot. Callers normalise once.
pub fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether a normalised name is inside `*.temps.local`.
pub fn in_zone(name: &str) -> bool {
    name == ZONE
        || name
            .strip_suffix(ZONE)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// What the resolver does with one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsDecision<'a> {
    /// Today's behaviour: answer `lookup` from the zone when it is inside
    /// it, forward otherwise.
    Unrestricted { lookup: &'a str },
    /// A restricted source asking for a name it may see: answer `lookup`
    /// from the zone, never forward.
    Answer { lookup: &'a str },
    /// A restricted source asking for anything else: NXDOMAIN, no forward.
    Refuse,
}

/// Names a restricted source may resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceView {
    pub workload: WorkloadRef,
    allowed: HashSet<Box<str>>,
}

impl SourceView {
    pub fn allows(&self, name: &str) -> bool {
        self.allowed.contains(name)
    }

    pub fn allowed_names(&self) -> impl Iterator<Item = &str> {
        self.allowed.iter().map(|n| &**n)
    }
}

/// One name and the workload that owns it (from `dns_name_claims`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NamedWorkload {
    pub fqdn: String,
    pub owner: WorkloadRef,
}

/// A group short name and the canonical name it stands for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alias {
    pub alias: String,
    pub canonical: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsPolicy {
    /// Only restricted sources appear here; absence means unrestricted.
    sources: HashMap<IpAddr, Arc<SourceView>>,
    aliases: HashMap<Box<str>, Box<str>>,
}

impl DnsPolicy {
    /// No restrictions and no aliases: today's resolver.
    pub fn open() -> Self {
        Self::default()
    }

    /// Builds the snapshot for the policies enforced on this node.
    ///
    /// A source is restricted when its egress profile is `restricted` or
    /// `none`. It may resolve the names of every workload an egress allow
    /// rule names (unless a port-less deny names the same workload), its
    /// own names, and the aliases of those names. `none` resolves nothing.
    pub fn build(
        policies: &[WorkloadPolicy],
        identities: &IdentityMap,
        names: &[NamedWorkload],
        aliases: &[Alias],
    ) -> Self {
        let mut names_by_owner: HashMap<WorkloadRef, Vec<String>> = HashMap::new();
        for named in names {
            names_by_owner
                .entry(named.owner)
                .or_default()
                .push(normalize(&named.fqdn));
        }
        let aliases: HashMap<Box<str>, Box<str>> = aliases
            .iter()
            .map(|a| (normalize(&a.alias).into(), normalize(&a.canonical).into()))
            .collect();
        let mut aliases_of: HashMap<&str, Vec<&str>> = HashMap::new();
        for (alias, canonical) in &aliases {
            aliases_of.entry(canonical).or_default().push(alias);
        }

        let mut sources = HashMap::new();
        for policy in policies {
            if policy.egress == Profile::Open {
                continue;
            }
            let mut allowed: HashSet<Box<str>> = HashSet::new();
            if policy.egress == Profile::Restricted {
                let denied: HashSet<WorkloadRef> = policy
                    .rules
                    .iter()
                    .filter(|r| {
                        r.direction == Direction::Egress
                            && r.action == Action::Deny
                            && r.ports.is_none()
                    })
                    .filter_map(|r| match r.peer {
                        Peer::Workload(w) => Some(w),
                        Peer::Cidr(_) => None,
                    })
                    .collect();
                let mut reachable: Vec<WorkloadRef> = vec![policy.subject];
                reachable.extend(policy.rules.iter().filter_map(|r| match r.peer {
                    Peer::Workload(w)
                        if r.direction == Direction::Egress
                            && r.action == Action::Allow
                            && !denied.contains(&w) =>
                    {
                        Some(w)
                    }
                    _ => None,
                }));
                for workload in reachable {
                    for name in names_by_owner.get(&workload).into_iter().flatten() {
                        for alias in aliases_of.get(name.as_str()).into_iter().flatten() {
                            allowed.insert((*alias).into());
                        }
                        allowed.insert(name.as_str().into());
                    }
                }
            }
            let view = Arc::new(SourceView {
                workload: policy.subject,
                allowed,
            });
            for addr in identities.addresses(&policy.subject) {
                sources.insert(addr, view.clone());
            }
        }
        Self { sources, aliases }
    }

    /// The decision for `name` (normalised: lower case, no trailing dot)
    /// asked by `source`. Two hash lookups; no allocation.
    pub fn decide<'a>(&'a self, source: IpAddr, name: &'a str) -> DnsDecision<'a> {
        let source = canonical_source(source);
        let lookup: &'a str = self.aliases.get(name).map(|c| &**c).unwrap_or(name);
        let Some(view) = self.sources.get(&source) else {
            return DnsDecision::Unrestricted { lookup };
        };
        if name == MARKER {
            return DnsDecision::Answer { lookup };
        }
        if in_zone(name) && (view.allows(name) || view.allows(lookup)) {
            DnsDecision::Answer { lookup }
        } else {
            DnsDecision::Refuse
        }
    }

    /// The view of a restricted source, for the console and for logs.
    pub fn source(&self, source: IpAddr) -> Option<&SourceView> {
        self.sources.get(&canonical_source(source)).map(|v| &**v)
    }

    pub fn restricted_sources(&self) -> usize {
        self.sources.len()
    }
}

/// An IPv4 client reaching a dual-stack socket shows up as `::ffff:a.b.c.d`.
fn canonical_source(source: IpAddr) -> IpAddr {
    match source {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{L4Protocol, Origin, PortRange, Rule};
    use temps_workload::WorkloadKind;

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }
    fn service(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Service, id)
    }
    fn deployment(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Deployment, id)
    }

    fn allow(peer: WorkloadRef) -> Rule {
        Rule {
            id: 1,
            direction: Direction::Egress,
            peer: Peer::Workload(peer),
            ports: Some(PortRange::single(5432)),
            protocol: L4Protocol::Tcp,
            action: Action::Allow,
            origin: Origin::Link,
        }
    }

    fn fixture(egress: Profile) -> DnsPolicy {
        let policy = WorkloadPolicy {
            subject: sandbox(1),
            egress,
            ingress: Profile::Open,
            rules: vec![allow(service(9))],
        };
        let ids = IdentityMap::new()
            .with(sandbox(1), "10.237.1.5".parse().unwrap())
            .with(deployment(3), "10.237.1.6".parse().unwrap());
        let names = [
            NamedWorkload {
                fqdn: "Shop-DB.temps.local.".into(),
                owner: service(9),
            },
            NamedWorkload {
                fqdn: "cache.temps.local".into(),
                owner: service(10),
            },
            NamedWorkload {
                fqdn: "sbx-1.sbx.temps.local".into(),
                owner: sandbox(1),
            },
        ];
        let aliases = [Alias {
            alias: group_short_name("db", "shop"),
            canonical: "shop-db.temps.local".into(),
        }];
        let open = WorkloadPolicy {
            subject: deployment(3),
            egress: Profile::Open,
            ingress: Profile::Open,
            rules: vec![],
        };
        DnsPolicy::build(&[policy, open], &ids, &names, &aliases)
    }

    #[test]
    fn a_restricted_source_resolves_only_what_it_may_reach() {
        let dns = fixture(Profile::Restricted);
        let me: IpAddr = "10.237.1.5".parse().unwrap();
        assert_eq!(
            dns.decide(me, "shop-db.temps.local"),
            DnsDecision::Answer {
                lookup: "shop-db.temps.local"
            }
        );
        assert_eq!(
            dns.decide(me, "sbx-1.sbx.temps.local"),
            DnsDecision::Answer {
                lookup: "sbx-1.sbx.temps.local"
            },
            "its own name"
        );
        assert_eq!(
            dns.decide(me, "cache.temps.local"),
            DnsDecision::Refuse,
            "an unlinked service is undiscoverable"
        );
        assert_eq!(
            dns.decide(me, "example.com"),
            DnsDecision::Refuse,
            "no external lookups"
        );
        assert_eq!(
            dns.decide(me, MARKER),
            DnsDecision::Answer { lookup: MARKER }
        );
    }

    #[test]
    fn group_short_names_are_aliases_that_follow_policy() {
        let dns = fixture(Profile::Restricted);
        let me: IpAddr = "10.237.1.5".parse().unwrap();
        assert_eq!(group_zone("Shop"), "shop.grp.temps.local");
        assert_eq!(
            dns.decide(me, "db.shop.grp.temps.local"),
            DnsDecision::Answer {
                lookup: "shop-db.temps.local"
            }
        );
        let open: IpAddr = "10.237.1.6".parse().unwrap();
        assert_eq!(
            dns.decide(open, "db.shop.grp.temps.local"),
            DnsDecision::Unrestricted {
                lookup: "shop-db.temps.local"
            },
            "aliases resolve for everyone"
        );
    }

    #[test]
    fn open_and_unknown_sources_keep_todays_behaviour() {
        let dns = fixture(Profile::Restricted);
        for source in ["10.237.1.6", "192.0.2.1", "::ffff:192.0.2.1"] {
            assert_eq!(
                dns.decide(source.parse().unwrap(), "example.com"),
                DnsDecision::Unrestricted {
                    lookup: "example.com"
                }
            );
        }
        assert_eq!(dns.restricted_sources(), 1);
    }

    #[test]
    fn a_mapped_ipv4_source_is_the_same_source() {
        let dns = fixture(Profile::Restricted);
        assert_eq!(
            dns.decide("::ffff:10.237.1.5".parse().unwrap(), "cache.temps.local"),
            DnsDecision::Refuse
        );
    }

    #[test]
    fn a_none_profile_resolves_nothing() {
        let dns = fixture(Profile::None);
        let me: IpAddr = "10.237.1.5".parse().unwrap();
        assert_eq!(dns.decide(me, "shop-db.temps.local"), DnsDecision::Refuse);
        assert_eq!(dns.decide(me, "sbx-1.sbx.temps.local"), DnsDecision::Refuse);
    }

    #[test]
    fn a_portless_deny_hides_the_peer() {
        let mut policy = WorkloadPolicy {
            subject: sandbox(1),
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![allow(service(9))],
        };
        policy.rules.push(Rule {
            id: 2,
            ports: None,
            action: Action::Deny,
            ..allow(service(9))
        });
        let ids = IdentityMap::new().with(sandbox(1), "10.237.1.5".parse().unwrap());
        let names = [NamedWorkload {
            fqdn: "shop-db.temps.local".into(),
            owner: service(9),
        }];
        let dns = DnsPolicy::build(&[policy], &ids, &names, &[]);
        assert_eq!(
            dns.decide("10.237.1.5".parse().unwrap(), "shop-db.temps.local"),
            DnsDecision::Refuse
        );
    }

    #[test]
    fn zone_membership_is_by_label() {
        assert!(in_zone("temps.local"));
        assert!(in_zone("a.temps.local"));
        assert!(!in_zone("eviltemps.local"));
        assert!(!in_zone("temps.local.example.com"));
        assert_eq!(normalize("A.Temps.Local."), "a.temps.local");
    }
}
