// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Policies and identities → the contents of the eBPF maps (network access
//! plan, §4.15).
//!
//! Pure and deterministic: the same input always yields the same plan,
//! whatever order policies and rules come in, so a pass can compare digests
//! and skip an unchanged apply.
//!
//! - **Identities.** A workload's identity is its kind code in the top four
//!   bits and its id below ([`identity_of`]). Every address prefix the
//!   policy needs (rule CIDRs, system peers, the metadata addresses and the
//!   built-in internal ranges) gets a prefix identity. Workload addresses
//!   are `/32` or `/128` entries; the longest match wins, so an address of a
//!   known workload is that workload, never a CIDR around it. A CIDR rule
//!   therefore matches destinations that are not known workloads, as in
//!   Cilium and the Kubernetes guidance for `ipBlock`.
//! - **A CIDR rule applies to every prefix inside it**, and to the world
//!   when it is `/0`. That is what makes a deny on `10.0.0.0/8` also cover a
//!   narrower allow on `10.1.0.0/16`.
//! - **Precedence (§4.9) is resolved here**, so a program does at most two
//!   lookups per subject: entries for every subject (system allows, then the
//!   metadata block), then the subject's own, where a deny already beats an
//!   allow for the same peer, protocol and port.
//! - **Port ranges** are expanded into exact keys up to
//!   [`MAX_EXPANDED_PORTS`]. `1-65535` means any port. A wider range is
//!   never silently dropped in the permissive direction: an allow is skipped
//!   (stricter than asked) and a deny is widened to every port (stricter
//!   too), each with a warning.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr};

use temps_netpol_ebpf_common::{
    Endpoint, PolicyKey, PolicyValue, DIR_EGRESS, DIR_INGRESS, ID_CLASS_PREFIX, ID_CLASS_SHIFT,
    ID_LOCAL_MASK, ID_WORLD, IPPROTO_TCP, IPPROTO_UDP, PROFILE_NONE, PROFILE_OPEN,
    PROFILE_RESTRICTED, SUBJECT_ANY, VERDICT_ALLOW, VERDICT_DENY,
};
use temps_workload::{WorkloadKind, WorkloadRef};

use crate::enforcer::SystemPeer;
use crate::identity::IdentityMap;
use crate::rule::{Action, Cidr, Direction, L4Protocol, Peer, PortRange, Profile, WorkloadPolicy};

/// Widest port range expanded into exact keys.
pub const MAX_EXPANDED_PORTS: u32 = 1024;

/// Ranges that are internal whatever the rules say: an address there that
/// no rule names is "unknown internal", never the internet, so `internet`
/// does not open it.
pub const INTERNAL_RANGES: [&str; 7] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "169.254.0.0/16",
    "fc00::/7",
    "fe80::/10",
];

/// Cloud metadata endpoints, refused to every workload (§4.9, step 2).
pub const METADATA_ADDRESSES: [&str; 2] = ["169.254.169.254/32", "fd00:ec2::254/128"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatapathCompileError {
    #[error(
        "workload {workload} has id {id}, above the largest the eBPF datapath can identify ({max})"
    )]
    IdentityOutOfRange {
        workload: WorkloadRef,
        id: i64,
        max: u32,
    },
    #[error("workload {subject} has two policies; a workload has exactly one")]
    DuplicateSubject { subject: WorkloadRef },
    #[error(
        "rule {rule_id} of {subject} has port range {start}-{end}: expected 1 <= start <= end"
    )]
    InvalidPorts {
        subject: WorkloadRef,
        rule_id: i64,
        start: u16,
        end: u16,
    },
    #[error("rule {rule_id} of {subject} names the subject itself as its peer")]
    SelfPeer { subject: WorkloadRef, rule_id: i64 },
    #[error("the policy needs {count} address prefixes, more than the eBPF datapath can identify ({max})")]
    TooManyPrefixes { count: usize, max: u32 },
}

/// Everything one node's maps are compiled from.
#[derive(Debug, Clone, Copy)]
pub struct DatapathInput<'a> {
    /// Policies with something to enforce; a workload without one is open.
    pub policies: &'a [WorkloadPolicy],
    /// Addresses of local and remote workloads.
    pub identities: &'a IdentityMap,
    /// Workloads with an instance on this node: each gets a
    /// `CGROUP_IDENTITY` entry, open or not, because an enforcing node
    /// refuses a workload slice it cannot identify.
    pub local: &'a BTreeSet<WorkloadRef>,
    /// Workloads whose restricted egress may reach the internet.
    pub internet: &'a BTreeSet<WorkloadRef>,
    /// Workloads in trial mode: evaluated and reported, never blocked.
    pub trial: &'a BTreeSet<WorkloadRef>,
    /// Addresses every workload may reach (the node resolver, the egress
    /// proxy), whatever its rules.
    pub system_egress: &'a [SystemPeer],
    /// Sources every workload accepts (the host and its proxy), whatever its
    /// ingress rules.
    pub system_ingress: &'a [Cidr],
}

/// The contents of the maps, keyed by workload where the loader still has to
/// resolve a cgroup id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DatapathPlan {
    pub endpoints: BTreeMap<WorkloadRef, Endpoint>,
    pub ipcache: BTreeMap<Cidr, u32>,
    pub policy: BTreeMap<PolicyKey, PolicyValue>,
    /// Rules that compile but cannot have their full effect, worded for an
    /// operator.
    pub warnings: Vec<String>,
}

impl DatapathPlan {
    /// Changes whenever any map content changes.
    pub fn digest(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.endpoints.hash(&mut hasher);
        self.ipcache.hash(&mut hasher);
        self.policy.hash(&mut hasher);
        hasher.finish()
    }
}

fn kind_code(kind: WorkloadKind) -> u32 {
    match kind {
        WorkloadKind::Deployment => 1,
        WorkloadKind::Sandbox => 2,
        WorkloadKind::Service => 3,
        WorkloadKind::Function => 4,
        WorkloadKind::Job => 5,
    }
}

/// The identity of a workload in the maps.
pub fn identity_of(workload: WorkloadRef) -> Result<u32, DatapathCompileError> {
    u32::try_from(workload.id)
        .ok()
        .filter(|id| *id <= ID_LOCAL_MASK)
        .map(|id| (kind_code(workload.kind) << ID_CLASS_SHIFT) | id)
        .ok_or(DatapathCompileError::IdentityOutOfRange {
            workload,
            id: workload.id,
            max: ID_LOCAL_MASK,
        })
}

fn prefix_identity(index: usize) -> Result<u32, DatapathCompileError> {
    u32::try_from(index)
        .ok()
        .filter(|i| *i <= ID_LOCAL_MASK)
        .map(|i| (ID_CLASS_PREFIX << ID_CLASS_SHIFT) | i)
        .ok_or(DatapathCompileError::TooManyPrefixes {
            count: index + 1,
            max: ID_LOCAL_MASK,
        })
}

fn builtin(cidr: &str) -> Cidr {
    // The constants above are valid blocks; the tests parse every one.
    cidr.parse()
        .unwrap_or_else(|_| Cidr::host(IpAddr::V4(Ipv4Addr::UNSPECIFIED)))
}

fn inside(outer: &Cidr, inner: &Cidr) -> bool {
    outer.is_ipv4() == inner.is_ipv4()
        && inner.prefix() >= outer.prefix()
        && outer.contains(inner.addr())
}

fn protocols(protocol: L4Protocol) -> &'static [u8] {
    match protocol {
        L4Protocol::Tcp => &[IPPROTO_TCP],
        L4Protocol::Udp => &[IPPROTO_UDP],
        L4Protocol::Any => &[IPPROTO_TCP, IPPROTO_UDP],
    }
}

fn dir_code(direction: Direction) -> u8 {
    match direction {
        Direction::Egress => DIR_EGRESS,
        Direction::Ingress => DIR_INGRESS,
    }
}

fn direction_word(direction: Direction) -> &'static str {
    match direction {
        Direction::Egress => "egress",
        Direction::Ingress => "ingress",
    }
}

fn profile_code(profile: Profile) -> u8 {
    match profile {
        Profile::Open => PROFILE_OPEN,
        Profile::Restricted => PROFILE_RESTRICTED,
        Profile::None => PROFILE_NONE,
    }
}

/// Which ports a rule covers once range limits are applied.
enum Ports {
    Any,
    Exact(Vec<u16>),
}

/// What all rules say about one (direction, subject, peer, protocol).
#[derive(Default)]
struct Group {
    deny_any: Option<u32>,
    deny_ports: BTreeMap<u16, u32>,
    allow_any: Option<u32>,
    allow_ports: BTreeMap<u16, u32>,
}

impl Group {
    fn add(&mut self, action: Action, ports: &Ports, rule: u32) {
        match (action, ports) {
            (Action::Deny, Ports::Any) => {
                self.deny_any.get_or_insert(rule);
            }
            (Action::Allow, Ports::Any) => {
                self.allow_any.get_or_insert(rule);
            }
            (Action::Deny, Ports::Exact(list)) => {
                for port in list {
                    self.deny_ports.entry(*port).or_insert(rule);
                }
            }
            (Action::Allow, Ports::Exact(list)) => {
                for port in list {
                    self.allow_ports.entry(*port).or_insert(rule);
                }
            }
        }
    }

    /// Port → verdict, with a deny beating an allow on the same port.
    fn entries(&self) -> Vec<(u16, PolicyValue)> {
        let value = |verdict, rule| PolicyValue {
            verdict,
            _pad: [0; 3],
            rule,
        };
        let mut out = Vec::new();
        let exact: BTreeSet<u16> = self
            .deny_ports
            .keys()
            .chain(self.allow_ports.keys())
            .copied()
            .collect();
        for port in exact {
            let entry = match (self.deny_ports.get(&port), self.deny_any) {
                (Some(rule), _) => value(VERDICT_DENY, *rule),
                (None, Some(rule)) => value(VERDICT_DENY, rule),
                (None, None) => match self.allow_ports.get(&port) {
                    Some(rule) => value(VERDICT_ALLOW, *rule),
                    None => continue,
                },
            };
            out.push((port, entry));
        }
        if let Some(rule) = self.deny_any {
            out.push((0, value(VERDICT_DENY, rule)));
        } else if let Some(rule) = self.allow_any {
            out.push((0, value(VERDICT_ALLOW, rule)));
        }
        out
    }
}

type GroupKey = (u8, u32, u32, u8);

fn rule_tag(id: i64) -> u32 {
    u32::try_from(id).unwrap_or(u32::MAX)
}

fn expand(ports: Option<PortRange>) -> Option<Ports> {
    match ports {
        None => Some(Ports::Any),
        Some(range) if range.start <= 1 && range.end == u16::MAX => Some(Ports::Any),
        Some(range) => {
            let width = u32::from(range.end) - u32::from(range.start) + 1;
            (width <= MAX_EXPANDED_PORTS).then(|| Ports::Exact((range.start..=range.end).collect()))
        }
    }
}

fn put(
    policy: &mut BTreeMap<PolicyKey, PolicyValue>,
    dir: u8,
    subject: u32,
    peer: u32,
    proto: u8,
    port: u16,
    value: PolicyValue,
) {
    policy.insert(
        PolicyKey {
            dir,
            proto,
            port,
            subject,
            peer,
        },
        value,
    );
}

/// Compiles one node's policies, identities and system peers into map
/// contents.
pub fn compile_datapath(input: DatapathInput<'_>) -> Result<DatapathPlan, DatapathCompileError> {
    let mut policies: Vec<&WorkloadPolicy> = input.policies.iter().collect();
    policies.sort_by_key(|p| p.subject);
    for pair in policies.windows(2) {
        if pair[0].subject == pair[1].subject {
            return Err(DatapathCompileError::DuplicateSubject {
                subject: pair[0].subject,
            });
        }
    }
    for policy in &policies {
        for rule in &policy.rules {
            if let Some(ports) = rule.ports {
                if !ports.is_valid() {
                    return Err(DatapathCompileError::InvalidPorts {
                        subject: policy.subject,
                        rule_id: rule.id,
                        start: ports.start,
                        end: ports.end,
                    });
                }
            }
            if rule.peer == Peer::Workload(policy.subject) {
                return Err(DatapathCompileError::SelfPeer {
                    subject: policy.subject,
                    rule_id: rule.id,
                });
            }
        }
    }

    let mut warnings = Vec::new();

    // Every prefix the maps need, each with its own identity.
    let metadata: Vec<Cidr> = METADATA_ADDRESSES.iter().map(|c| builtin(c)).collect();
    let mut prefixes: BTreeSet<Cidr> = INTERNAL_RANGES.iter().map(|c| builtin(c)).collect();
    prefixes.extend(metadata.iter().copied());
    prefixes.extend(input.system_egress.iter().map(|p| p.cidr));
    prefixes.extend(input.system_ingress.iter().copied());
    for policy in &policies {
        for rule in &policy.rules {
            if let Peer::Cidr(cidr) = rule.peer {
                prefixes.insert(cidr);
            }
        }
    }
    let mut prefix_ids: BTreeMap<Cidr, u32> = BTreeMap::new();
    for (index, prefix) in prefixes.iter().enumerate() {
        prefix_ids.insert(*prefix, prefix_identity(index)?);
    }
    // The identities a CIDR covers: every prefix inside it, and the world
    // when it covers everything.
    let covered = |cidr: &Cidr| -> Vec<u32> {
        let mut ids: Vec<u32> = prefix_ids
            .iter()
            .filter(|(prefix, _)| inside(cidr, prefix))
            .map(|(_, id)| *id)
            .collect();
        if cidr.prefix() == 0 {
            ids.push(ID_WORLD);
        }
        ids
    };

    let mut ipcache: BTreeMap<Cidr, u32> = prefix_ids.clone();
    let mut workloads: BTreeSet<WorkloadRef> = input.identities.workloads().copied().collect();
    workloads.extend(input.local.iter().copied());
    let mut claimed: BTreeMap<IpAddr, Vec<WorkloadRef>> = BTreeMap::new();
    for workload in &workloads {
        for addr in input.identities.addresses(workload) {
            claimed.entry(addr).or_default().push(*workload);
        }
    }
    for (addr, owners) in &claimed {
        match owners.as_slice() {
            [owner] => {
                ipcache.insert(Cidr::host(*addr), identity_of(*owner)?);
            }
            many => {
                let names: Vec<String> = many.iter().map(|w| w.to_string()).collect();
                warnings.push(format!(
                    "address {addr} is claimed by {}; it is left unidentified until one of them releases it",
                    names.join(" and ")
                ));
            }
        }
    }

    let mut policy: BTreeMap<PolicyKey, PolicyValue> = BTreeMap::new();
    let builtin_value = |verdict| PolicyValue {
        verdict,
        _pad: [0; 3],
        rule: 0,
    };

    // Step 1 of §4.9: system allows, for every subject.
    for peer in input.system_egress {
        let ports = match expand(peer.ports) {
            Some(ports) => ports,
            None => {
                warnings.push(format!(
                    "system peer {} has a port range wider than {MAX_EXPANDED_PORTS} ports; it is allowed on every port",
                    peer.cidr
                ));
                Ports::Any
            }
        };
        for id in covered(&peer.cidr) {
            for proto in protocols(peer.protocol) {
                match &ports {
                    Ports::Any => put(
                        &mut policy,
                        DIR_EGRESS,
                        SUBJECT_ANY,
                        id,
                        *proto,
                        0,
                        builtin_value(VERDICT_ALLOW),
                    ),
                    Ports::Exact(list) => {
                        for port in list {
                            put(
                                &mut policy,
                                DIR_EGRESS,
                                SUBJECT_ANY,
                                id,
                                *proto,
                                *port,
                                builtin_value(VERDICT_ALLOW),
                            );
                        }
                    }
                }
            }
        }
    }
    for cidr in input.system_ingress {
        for id in covered(cidr) {
            for proto in [IPPROTO_TCP, IPPROTO_UDP] {
                put(
                    &mut policy,
                    DIR_INGRESS,
                    SUBJECT_ANY,
                    id,
                    proto,
                    0,
                    builtin_value(VERDICT_ALLOW),
                );
            }
        }
    }
    // Step 2: the metadata block, unless a system allow already names the
    // same identity and port.
    for cidr in &metadata {
        for id in covered(cidr) {
            for proto in [IPPROTO_TCP, IPPROTO_UDP] {
                let key = PolicyKey {
                    dir: DIR_EGRESS,
                    proto,
                    port: 0,
                    subject: SUBJECT_ANY,
                    peer: id,
                };
                if !policy.contains_key(&key) {
                    put(
                        &mut policy,
                        DIR_EGRESS,
                        SUBJECT_ANY,
                        id,
                        proto,
                        0,
                        builtin_value(VERDICT_DENY),
                    );
                }
            }
        }
    }

    // Steps 3 and 4: the subjects' own rules.
    let mut groups: BTreeMap<GroupKey, Group> = BTreeMap::new();
    let mut by_subject: BTreeMap<WorkloadRef, &WorkloadPolicy> = BTreeMap::new();
    for policy_of in &policies {
        by_subject.insert(policy_of.subject, policy_of);
        let subject = identity_of(policy_of.subject)?;
        let mut rules: Vec<_> = policy_of.rules.iter().collect();
        rules.sort_by_key(|r| r.id);
        for rule in rules {
            let profile = policy_of.profile(rule.direction);
            if rule.action == Action::Allow && profile != Profile::Restricted {
                let reason = if profile == Profile::None {
                    "is 'none': nothing passes"
                } else {
                    "is 'open': it has no effect until the profile is 'restricted'"
                };
                warnings.push(format!(
                    "rule {} of {} allows {} traffic, but the {} profile {reason}",
                    rule.id,
                    policy_of.subject,
                    direction_word(rule.direction),
                    direction_word(rule.direction),
                ));
                continue;
            }
            let ports = match (expand(rule.ports), rule.action) {
                (Some(ports), _) => ports,
                (None, Action::Allow) => {
                    warnings.push(format!(
                        "rule {} of {} allows a port range wider than {MAX_EXPANDED_PORTS} ports; it is not applied. Split it, or allow every port",
                        rule.id, policy_of.subject
                    ));
                    continue;
                }
                (None, Action::Deny) => {
                    warnings.push(format!(
                        "rule {} of {} denies a port range wider than {MAX_EXPANDED_PORTS} ports; it is applied to every port",
                        rule.id, policy_of.subject
                    ));
                    Ports::Any
                }
            };
            let peers = match rule.peer {
                Peer::Workload(peer) => vec![identity_of(peer)?],
                Peer::Cidr(cidr) => covered(&cidr),
            };
            let dir = dir_code(rule.direction);
            for peer in peers {
                for proto in protocols(rule.protocol) {
                    groups.entry((dir, subject, peer, *proto)).or_default().add(
                        rule.action,
                        &ports,
                        rule_tag(rule.id),
                    );
                }
            }
        }
    }
    let mut with_entries: BTreeSet<(u8, u32)> = BTreeSet::new();
    for ((dir, subject, peer, proto), group) in &groups {
        for (port, value) in group.entries() {
            put(&mut policy, *dir, *subject, *peer, *proto, port, value);
            with_entries.insert((*dir, *subject));
        }
    }

    let mut endpoints = BTreeMap::new();
    for workload in input.local {
        let identity = identity_of(*workload)?;
        let (egress, ingress) = by_subject
            .get(workload)
            .map(|p| (p.egress, p.ingress))
            .unwrap_or_default();
        endpoints.insert(
            *workload,
            Endpoint {
                identity,
                egress: profile_code(egress),
                ingress: profile_code(ingress),
                internet: u8::from(input.internet.contains(workload)),
                trial: u8::from(input.trial.contains(workload)),
                egress_entries: u8::from(with_entries.contains(&(DIR_EGRESS, identity))),
                ingress_entries: u8::from(with_entries.contains(&(DIR_INGRESS, identity))),
                _pad: [0; 2],
            },
        );
    }

    Ok(DatapathPlan {
        endpoints,
        ipcache,
        policy,
        warnings,
    })
}

/// The bytes of a prefix's address, network order, as the LPM keys hold it.
pub fn prefix_bytes_v4(cidr: &Cidr) -> Option<[u8; 4]> {
    match cidr.addr() {
        IpAddr::V4(v4) => Some(v4.octets()),
        IpAddr::V6(_) => None,
    }
}

/// See [`prefix_bytes_v4`].
pub fn prefix_bytes_v6(cidr: &Cidr) -> Option<[u8; 16]> {
    match cidr.addr() {
        IpAddr::V6(v6) => Some(v6.octets()),
        IpAddr::V4(_) => None,
    }
}

/// An identity as an operator reads it: `sandbox/7`, `prefix#3`, `world`.
pub fn describe_identity(identity: u32) -> String {
    let class = identity >> ID_CLASS_SHIFT;
    let local = identity & ID_LOCAL_MASK;
    let kind = match class {
        0 if identity == ID_WORLD => return "world".to_string(),
        0 if identity == SUBJECT_ANY => return "any".to_string(),
        1 => "deployment",
        2 => "sandbox",
        3 => "service",
        4 => "function",
        5 => "job",
        c if c == ID_CLASS_PREFIX => return format!("prefix#{local}"),
        _ => return format!("unknown#{identity:#x}"),
    };
    format!("{kind}/{local}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Origin, Rule};

    fn w(kind: WorkloadKind, id: i64) -> WorkloadRef {
        WorkloadRef::new(kind, id)
    }

    fn rule(
        id: i64,
        direction: Direction,
        peer: Peer,
        ports: Option<PortRange>,
        action: Action,
    ) -> Rule {
        Rule {
            id,
            direction,
            peer,
            ports,
            protocol: L4Protocol::Tcp,
            action,
            origin: Origin::Operator,
        }
    }

    fn cidr(s: &str) -> Cidr {
        s.parse().unwrap()
    }

    struct Fixture {
        policies: Vec<WorkloadPolicy>,
        identities: IdentityMap,
        local: BTreeSet<WorkloadRef>,
        internet: BTreeSet<WorkloadRef>,
        trial: BTreeSet<WorkloadRef>,
        system_egress: Vec<SystemPeer>,
        system_ingress: Vec<Cidr>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                policies: Vec::new(),
                identities: IdentityMap::new(),
                local: BTreeSet::new(),
                internet: BTreeSet::new(),
                trial: BTreeSet::new(),
                system_egress: Vec::new(),
                system_ingress: Vec::new(),
            }
        }

        fn compile(&self) -> Result<DatapathPlan, DatapathCompileError> {
            compile_datapath(DatapathInput {
                policies: &self.policies,
                identities: &self.identities,
                local: &self.local,
                internet: &self.internet,
                trial: &self.trial,
                system_egress: &self.system_egress,
                system_ingress: &self.system_ingress,
            })
        }
    }

    fn key(dir: u8, subject: u32, peer: u32, proto: u8, port: u16) -> PolicyKey {
        PolicyKey {
            dir,
            proto,
            port,
            subject,
            peer,
        }
    }

    #[test]
    fn builtin_constants_parse() {
        for c in INTERNAL_RANGES.iter().chain(METADATA_ADDRESSES.iter()) {
            assert!(c.parse::<Cidr>().is_ok(), "{c} does not parse");
        }
    }

    #[test]
    fn identities_encode_kind_and_id() {
        assert_eq!(
            identity_of(w(WorkloadKind::Sandbox, 7)).unwrap(),
            (2 << 28) | 7
        );
        assert_eq!(
            describe_identity(identity_of(w(WorkloadKind::Service, 9)).unwrap()),
            "service/9"
        );
        assert_eq!(describe_identity(ID_WORLD), "world");
        assert!(matches!(
            identity_of(w(WorkloadKind::Job, 1 << 28)),
            Err(DatapathCompileError::IdentityOutOfRange { id, .. }) if id == 1 << 28
        ));
        assert!(identity_of(w(WorkloadKind::Job, -1)).is_err());
    }

    #[test]
    fn an_open_local_workload_gets_an_endpoint_and_no_entries() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Deployment, 3);
        f.local.insert(app);
        f.identities.insert(app, "10.202.1.5".parse().unwrap());
        let plan = f.compile().unwrap();
        let ep = plan.endpoints[&app];
        assert_eq!(ep.egress, PROFILE_OPEN);
        assert_eq!(ep.ingress, PROFILE_OPEN);
        assert_eq!(ep.egress_entries, 0);
        assert_eq!(ep.ingress_entries, 0);
        assert_eq!(plan.ipcache[&cidr("10.202.1.5/32")], ep.identity);
        assert!(plan.policy.keys().all(|k| k.subject == SUBJECT_ANY));
    }

    #[test]
    fn a_link_allows_one_port_of_one_service() {
        let mut f = Fixture::new();
        let sandbox = w(WorkloadKind::Sandbox, 7);
        let db = w(WorkloadKind::Service, 9);
        f.local.insert(sandbox);
        f.identities.insert(db, "10.202.2.9".parse().unwrap());
        f.policies.push(WorkloadPolicy {
            subject: sandbox,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![rule(
                1,
                Direction::Egress,
                Peer::Workload(db),
                Some(PortRange::single(5432)),
                Action::Allow,
            )],
        });
        let plan = f.compile().unwrap();
        let s = identity_of(sandbox).unwrap();
        let d = identity_of(db).unwrap();
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, d, IPPROTO_TCP, 5432)].verdict,
            VERDICT_ALLOW
        );
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, d, IPPROTO_TCP, 5432)].rule,
            1
        );
        assert!(!plan
            .policy
            .contains_key(&key(DIR_EGRESS, s, d, IPPROTO_TCP, 0)));
        assert_eq!(plan.endpoints[&sandbox].egress, PROFILE_RESTRICTED);
        assert_eq!(plan.endpoints[&sandbox].egress_entries, 1);
        assert_eq!(plan.endpoints[&sandbox].internet, 0);
    }

    #[test]
    fn a_deny_on_a_wide_cidr_beats_an_allow_inside_it() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Deployment, 1);
        f.local.insert(app);
        f.policies.push(WorkloadPolicy {
            subject: app,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![
                rule(
                    2,
                    Direction::Egress,
                    Peer::Cidr(cidr("10.1.0.0/16")),
                    Some(PortRange::single(443)),
                    Action::Allow,
                ),
                rule(
                    3,
                    Direction::Egress,
                    Peer::Cidr(cidr("10.0.0.0/8")),
                    None,
                    Action::Deny,
                ),
            ],
        });
        let plan = f.compile().unwrap();
        let s = identity_of(app).unwrap();
        let narrow = plan.ipcache[&cidr("10.1.0.0/16")];
        let entry = plan.policy[&key(DIR_EGRESS, s, narrow, IPPROTO_TCP, 443)];
        assert_eq!(entry.verdict, VERDICT_DENY);
        assert_eq!(entry.rule, 3);
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, narrow, IPPROTO_TCP, 0)].verdict,
            VERDICT_DENY
        );
    }

    #[test]
    fn an_exact_deny_carves_a_port_out_of_an_any_port_allow() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Deployment, 1);
        let db = w(WorkloadKind::Service, 2);
        f.local.insert(app);
        f.policies.push(WorkloadPolicy {
            subject: app,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![
                rule(
                    4,
                    Direction::Egress,
                    Peer::Workload(db),
                    None,
                    Action::Allow,
                ),
                rule(
                    5,
                    Direction::Egress,
                    Peer::Workload(db),
                    Some(PortRange::single(22)),
                    Action::Deny,
                ),
            ],
        });
        let plan = f.compile().unwrap();
        let (s, d) = (identity_of(app).unwrap(), identity_of(db).unwrap());
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, d, IPPROTO_TCP, 0)].verdict,
            VERDICT_ALLOW
        );
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, d, IPPROTO_TCP, 22)].verdict,
            VERDICT_DENY
        );
    }

    #[test]
    fn a_cidr_rule_covers_built_in_ranges_inside_it_and_the_world_only_at_slash_zero() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Function, 4);
        f.local.insert(app);
        f.policies.push(WorkloadPolicy {
            subject: app,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![rule(
                6,
                Direction::Egress,
                Peer::Cidr(cidr("0.0.0.0/0")),
                Some(PortRange::single(443)),
                Action::Allow,
            )],
        });
        let plan = f.compile().unwrap();
        let s = identity_of(app).unwrap();
        assert!(plan
            .policy
            .contains_key(&key(DIR_EGRESS, s, ID_WORLD, IPPROTO_TCP, 443)));
        let private = plan.ipcache[&cidr("192.168.0.0/16")];
        assert!(plan
            .policy
            .contains_key(&key(DIR_EGRESS, s, private, IPPROTO_TCP, 443)));
        // An IPv4 rule never covers IPv6 prefixes.
        let ula = plan.ipcache[&cidr("fc00::/7")];
        assert!(!plan
            .policy
            .contains_key(&key(DIR_EGRESS, s, ula, IPPROTO_TCP, 443)));
    }

    #[test]
    fn system_peers_are_allowed_for_everyone_and_metadata_is_refused() {
        let mut f = Fixture::new();
        f.system_egress.push(SystemPeer {
            cidr: cidr("10.202.1.1/32"),
            ports: Some(PortRange::single(53)),
            protocol: L4Protocol::Any,
        });
        f.system_ingress.push(cidr("10.202.1.1/32"));
        let plan = f.compile().unwrap();
        let resolver = plan.ipcache[&cidr("10.202.1.1/32")];
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, SUBJECT_ANY, resolver, IPPROTO_UDP, 53)].verdict,
            VERDICT_ALLOW
        );
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, SUBJECT_ANY, resolver, IPPROTO_TCP, 53)].verdict,
            VERDICT_ALLOW
        );
        assert_eq!(
            plan.policy[&key(DIR_INGRESS, SUBJECT_ANY, resolver, IPPROTO_TCP, 0)].verdict,
            VERDICT_ALLOW
        );
        let metadata = plan.ipcache[&cidr("169.254.169.254/32")];
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, SUBJECT_ANY, metadata, IPPROTO_TCP, 0)].verdict,
            VERDICT_DENY
        );
    }

    #[test]
    fn wide_port_ranges_fail_towards_stricter() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Deployment, 1);
        let db = w(WorkloadKind::Service, 2);
        f.local.insert(app);
        f.policies.push(WorkloadPolicy {
            subject: app,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![
                rule(
                    7,
                    Direction::Egress,
                    Peer::Workload(db),
                    Some(PortRange {
                        start: 1000,
                        end: 5000,
                    }),
                    Action::Allow,
                ),
                rule(
                    8,
                    Direction::Egress,
                    Peer::Cidr(cidr("10.9.0.0/16")),
                    Some(PortRange {
                        start: 1000,
                        end: 5000,
                    }),
                    Action::Deny,
                ),
                rule(
                    9,
                    Direction::Egress,
                    Peer::Cidr(cidr("10.8.0.0/16")),
                    Some(PortRange {
                        start: 1,
                        end: 65535,
                    }),
                    Action::Allow,
                ),
            ],
        });
        let plan = f.compile().unwrap();
        let (s, d) = (identity_of(app).unwrap(), identity_of(db).unwrap());
        assert!(plan.policy.keys().all(|k| !(k.subject == s && k.peer == d)));
        let denied = plan.ipcache[&cidr("10.9.0.0/16")];
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, denied, IPPROTO_TCP, 0)].verdict,
            VERDICT_DENY
        );
        let any = plan.ipcache[&cidr("10.8.0.0/16")];
        assert_eq!(
            plan.policy[&key(DIR_EGRESS, s, any, IPPROTO_TCP, 0)].verdict,
            VERDICT_ALLOW
        );
        assert_eq!(plan.warnings.len(), 2, "{:?}", plan.warnings);
    }

    #[test]
    fn allow_rules_under_open_or_none_have_no_entries_and_warn() {
        let mut f = Fixture::new();
        let a = w(WorkloadKind::Sandbox, 1);
        let b = w(WorkloadKind::Sandbox, 2);
        let db = w(WorkloadKind::Service, 3);
        f.local.extend([a, b]);
        f.policies.push(WorkloadPolicy {
            subject: a,
            egress: Profile::Open,
            ingress: Profile::Open,
            rules: vec![rule(
                10,
                Direction::Egress,
                Peer::Workload(db),
                None,
                Action::Allow,
            )],
        });
        f.policies.push(WorkloadPolicy {
            subject: b,
            egress: Profile::None,
            ingress: Profile::Open,
            rules: vec![rule(
                11,
                Direction::Egress,
                Peer::Workload(db),
                None,
                Action::Allow,
            )],
        });
        let plan = f.compile().unwrap();
        assert!(plan.policy.keys().all(|k| k.subject == SUBJECT_ANY));
        assert_eq!(plan.warnings.len(), 2);
        assert_eq!(plan.endpoints[&b].egress, PROFILE_NONE);
    }

    #[test]
    fn any_protocol_writes_tcp_and_udp() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Deployment, 1);
        let db = w(WorkloadKind::Service, 2);
        f.local.insert(app);
        let mut r = rule(
            12,
            Direction::Egress,
            Peer::Workload(db),
            Some(PortRange::single(53)),
            Action::Allow,
        );
        r.protocol = L4Protocol::Any;
        f.policies.push(WorkloadPolicy {
            subject: app,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![r],
        });
        let plan = f.compile().unwrap();
        let (s, d) = (identity_of(app).unwrap(), identity_of(db).unwrap());
        assert!(plan
            .policy
            .contains_key(&key(DIR_EGRESS, s, d, IPPROTO_TCP, 53)));
        assert!(plan
            .policy
            .contains_key(&key(DIR_EGRESS, s, d, IPPROTO_UDP, 53)));
    }

    #[test]
    fn an_address_claimed_twice_is_left_unidentified() {
        let mut f = Fixture::new();
        let a = w(WorkloadKind::Sandbox, 1);
        let b = w(WorkloadKind::Sandbox, 2);
        let ip: IpAddr = "10.202.1.9".parse().unwrap();
        f.identities.insert(a, ip).insert(b, ip);
        let plan = f.compile().unwrap();
        assert!(!plan.ipcache.contains_key(&Cidr::host(ip)));
        assert!(
            plan.warnings[0].contains("10.202.1.9"),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn internet_and_trial_flags_reach_the_endpoint() {
        let mut f = Fixture::new();
        let app = w(WorkloadKind::Sandbox, 5);
        f.local.insert(app);
        f.internet.insert(app);
        f.trial.insert(app);
        let plan = f.compile().unwrap();
        assert_eq!(plan.endpoints[&app].internet, 1);
        assert_eq!(plan.endpoints[&app].trial, 1);
    }

    #[test]
    fn the_plan_does_not_depend_on_input_order() {
        let a = w(WorkloadKind::Deployment, 1);
        let b = w(WorkloadKind::Deployment, 2);
        let db = w(WorkloadKind::Service, 3);
        let policy = |subject, ids: [i64; 2]| WorkloadPolicy {
            subject,
            egress: Profile::Restricted,
            ingress: Profile::Restricted,
            rules: vec![
                rule(
                    ids[0],
                    Direction::Egress,
                    Peer::Workload(db),
                    Some(PortRange::single(5432)),
                    Action::Allow,
                ),
                rule(
                    ids[1],
                    Direction::Ingress,
                    Peer::Cidr(cidr("10.0.0.0/8")),
                    Some(PortRange::single(80)),
                    Action::Allow,
                ),
            ],
        };
        let mut one = Fixture::new();
        one.local.extend([a, b]);
        one.policies = vec![policy(a, [1, 2]), policy(b, [3, 4])];
        let mut two = Fixture::new();
        two.local.extend([b, a]);
        let mut reversed = vec![policy(b, [3, 4]), policy(a, [1, 2])];
        for p in &mut reversed {
            p.rules.reverse();
        }
        two.policies = reversed;
        let (x, y) = (one.compile().unwrap(), two.compile().unwrap());
        assert_eq!(x, y);
        assert_eq!(x.digest(), y.digest());
    }

    #[test]
    fn validation_errors_name_the_rule() {
        let app = w(WorkloadKind::Deployment, 1);
        let mut f = Fixture::new();
        f.policies.push(WorkloadPolicy {
            subject: app,
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![rule(
                13,
                Direction::Egress,
                Peer::Workload(app),
                None,
                Action::Allow,
            )],
        });
        assert_eq!(
            f.compile(),
            Err(DatapathCompileError::SelfPeer {
                subject: app,
                rule_id: 13
            })
        );
        f.policies[0].rules[0] = rule(
            14,
            Direction::Egress,
            Peer::Cidr(cidr("10.0.0.0/8")),
            Some(PortRange { start: 9, end: 2 }),
            Action::Allow,
        );
        assert!(matches!(
            f.compile(),
            Err(DatapathCompileError::InvalidPorts { rule_id: 14, .. })
        ));
        f.policies.push(f.policies[0].clone());
        assert!(matches!(
            f.compile(),
            Err(DatapathCompileError::DuplicateSubject { .. })
        ));
    }
}
