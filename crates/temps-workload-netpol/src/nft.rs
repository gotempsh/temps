// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Policies and identities → one nftables ruleset.
//!
//! The ruleset is a single `table inet <name>`, replaced in one transaction
//! (`add`, `delete`, `add` in a single `nft -f` run), so a half-applied
//! policy is never visible and a rejected ruleset leaves the previous one
//! in force.
//!
//! Shape, for a subject `sandbox/1` restricted to `service/9` on 5432:
//!
//! ```text
//! table inet temps_netpol {
//!   set w_sandbox_1_v4 { type ipv4_addr; elements = { 172.20.0.5 } }
//!   set w_service_9_v4 { type ipv4_addr; elements = { 172.20.0.9 } }
//!   chain egress_sandbox_1 {
//!     ct state established,related return
//!     ip daddr @w_service_9_v4 tcp dport 5432 return
//!     counter reject with icmpx type admin-prohibited
//!   }
//!   chain forward {
//!     type filter hook forward priority -190; policy accept;
//!     ip saddr @w_sandbox_1_v4 jump egress_sandbox_1
//!   }
//! }
//! ```
//!
//! - **Order inside a direction chain**: replies of an allowed flow first,
//!   then every deny, then every allow, then the profile's default. A deny
//!   always beats an allow, whatever the rule ids.
//! - **Both directions are checked**: an egress chain that lets a packet
//!   through returns to `forward`, which then runs the destination's
//!   ingress chain. Traffic between two workloads needs the sender's
//!   egress and the receiver's ingress to agree.
//! - **Priority -190**: after the metadata guard (-200), before the mesh
//!   (-110), the overlay (-100) and Docker's own filter (0). An `accept`
//!   here does not skip those; only a `reject` is final.
//! - **The `input` hook carries egress too**: a container reaching an
//!   address of the host itself (its bridge gateway) is not forwarded, so a
//!   restricted workload is checked there as well.
//! - **What is not covered**: traffic the host originates (the proxy, the
//!   health checks) passes `output`, not `forward`, so ingress isolation
//!   never cuts the host off from its own workloads. Flows established
//!   before a deny is applied keep running until they close.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};

use temps_workload::WorkloadRef;

use crate::identity::IdentityMap;
use crate::rule::{Action, Direction, L4Protocol, Peer, PortRange, Profile, Rule, WorkloadPolicy};

/// Priority of the policy's base chains (see the module documentation).
pub const HOOK_PRIORITY: i32 = -190;

/// An nftables table name, `[a-z][a-z0-9_]{0,31}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableName(String);

impl TableName {
    /// The table the plane owns on every node.
    pub fn default_table() -> Self {
        Self("temps_netpol".to_string())
    }

    pub fn new(name: impl Into<String>) -> Result<Self, CompileError> {
        let name = name.into();
        let mut chars = name.chars();
        let valid = name.len() <= 32
            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if valid {
            Ok(Self(name))
        } else {
            Err(CompileError::InvalidTableName { name })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    #[error("'{name}' is not a valid nftables table name: expected [a-z][a-z0-9_]{{0,31}}")]
    InvalidTableName { name: String },
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
}

/// A compiled ruleset and what the compiler noticed on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub table: TableName,
    /// The `nft -f` script: replaces the whole table atomically.
    pub ruleset: String,
    /// Rules that compile but cannot have an effect, worded for an
    /// operator (for example an allow rule under a `none` profile).
    pub warnings: Vec<String>,
}

/// Compiles every policy enforced on a node into one ruleset.
///
/// Policies that are open in both directions and have no rules produce
/// nothing. The output is deterministic: the same input always yields the
/// same bytes, whatever order the policies and rules come in.
pub fn compile(
    table: &TableName,
    policies: &[WorkloadPolicy],
    identities: &IdentityMap,
) -> Result<Compiled, CompileError> {
    let mut sorted: Vec<&WorkloadPolicy> = policies.iter().collect();
    sorted.sort_by_key(|p| p.subject);
    for pair in sorted.windows(2) {
        if pair[0].subject == pair[1].subject {
            return Err(CompileError::DuplicateSubject {
                subject: pair[0].subject,
            });
        }
    }
    for policy in &sorted {
        for rule in &policy.rules {
            if let Some(ports) = rule.ports {
                if !ports.is_valid() {
                    return Err(CompileError::InvalidPorts {
                        subject: policy.subject,
                        rule_id: rule.id,
                        start: ports.start,
                        end: ports.end,
                    });
                }
            }
            if rule.peer == Peer::Workload(policy.subject) {
                return Err(CompileError::SelfPeer {
                    subject: policy.subject,
                    rule_id: rule.id,
                });
            }
        }
    }

    let enforced: Vec<&WorkloadPolicy> = sorted.into_iter().filter(|p| !p.is_noop()).collect();
    let mut warnings = Vec::new();

    // Every workload a set is needed for: subjects and workload peers.
    let mut workloads: BTreeSet<WorkloadRef> = BTreeSet::new();
    for policy in &enforced {
        workloads.insert(policy.subject);
        for rule in &policy.rules {
            if let Peer::Workload(peer) = rule.peer {
                workloads.insert(peer);
            }
        }
    }

    let mut body = String::new();
    for workload in &workloads {
        write_sets(&mut body, workload, identities);
    }
    for policy in &enforced {
        for direction in [Direction::Egress, Direction::Ingress] {
            if needs_chain(policy, direction) {
                write_direction_chain(&mut body, policy, direction, &mut warnings);
            }
        }
    }

    // Dispatch: `forward` for both directions, `input` for egress to the
    // host itself.
    let mut forward = String::new();
    let mut input = String::new();
    for policy in &enforced {
        let name = set_base(&policy.subject);
        if needs_chain(policy, Direction::Egress) {
            let chain = chain_name(&policy.subject, Direction::Egress);
            for line in [
                format!("ip saddr @{name}_v4 jump {chain}"),
                format!("ip6 saddr @{name}_v6 jump {chain}"),
            ] {
                let _ = writeln!(forward, "\t\t{line}");
                let _ = writeln!(input, "\t\t{line}");
            }
        }
        if needs_chain(policy, Direction::Ingress) {
            let chain = chain_name(&policy.subject, Direction::Ingress);
            let _ = writeln!(forward, "\t\tip daddr @{name}_v4 jump {chain}");
            let _ = writeln!(forward, "\t\tip6 daddr @{name}_v6 jump {chain}");
        }
    }
    let _ = writeln!(
        body,
        "\tchain forward {{\n\t\ttype filter hook forward priority {HOOK_PRIORITY}; policy accept;\n{forward}\t}}"
    );
    let _ = writeln!(
        body,
        "\tchain input {{\n\t\ttype filter hook input priority {HOOK_PRIORITY}; policy accept;\n{input}\t}}"
    );

    let mut ruleset = String::new();
    let _ = writeln!(
        ruleset,
        "# Generated by temps-workload-netpol. Do not edit: the next apply replaces it."
    );
    let _ = writeln!(ruleset, "table inet {table}");
    let _ = writeln!(ruleset, "delete table inet {table}");
    let _ = write!(ruleset, "table inet {table} {{\n{body}}}\n");

    Ok(Compiled {
        table: table.clone(),
        ruleset,
        warnings,
    })
}

/// Ports a workload may still open on a node's host address: the public
/// ingress, which routes by name and so applies every policy itself.
pub const NODE_INGRESS_PORTS: [u16; 2] = [80, 443];

/// The table holding the host-port rules (see [`host_port_ruleset`]).
pub fn host_port_table(table: &TableName) -> TableName {
    TableName::new(format!("{}_hp", table.0))
        .unwrap_or_else(|_| TableName("temps_netpol_hp".into()))
}

/// Rules that keep this node's workloads from reaching any node's published
/// ports through a host address.
///
/// A connection from a workload to `<node address>:<published port>` is
/// masqueraded to this node's address on the way out, and every node trusts
/// node addresses as system sources (the proxies and health checks dial from
/// them), so the receiving workload's ingress rules never saw the real
/// sender. Rejected here instead, where the sender is still known:
///
/// - toward another node's host address, on any port but the public
///   ingress's;
/// - toward this node's own published ports: a hairpin is DNAT'd before the
///   policy sees it, so it is recognised by its conntrack entry.
///
/// Workloads reach each other by their `*.temps.local` names, over the
/// overlay, where the real source address arrives and policy applies. Only
/// new connections are refused; replies always pass.
pub fn host_port_ruleset(
    table: &TableName,
    workloads: &BTreeSet<std::net::IpAddr>,
    node_hosts: &[crate::rule::Cidr],
) -> String {
    let table = host_port_table(table);
    if workloads.is_empty() {
        return removal_ruleset(&table);
    }
    let ports = NODE_INGRESS_PORTS
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut body = String::new();
    let mut rules = String::new();
    for (family, v4) in [("ip", true), ("ip6", false)] {
        let sources: Vec<String> = workloads
            .iter()
            .filter(|a| a.is_ipv4() == v4)
            .map(ToString::to_string)
            .collect();
        if sources.is_empty() {
            continue;
        }
        let (kind, suffix) = if v4 {
            ("ipv4_addr", "v4")
        } else {
            ("ipv6_addr", "v6")
        };
        let _ = writeln!(
            body,
            "\tset workloads_{suffix} {{ type {kind}; elements = {{ {} }} }}",
            sources.join(", ")
        );
        let _ = writeln!(
            rules,
            "\t\tct state new {family} saddr @workloads_{suffix} ct status dnat meta l4proto {{ tcp, udp }} ct original proto-dst != {{ {ports} }} counter reject with icmpx type admin-prohibited"
        );
        let hosts: Vec<String> = node_hosts
            .iter()
            .filter(|c| c.addr().is_ipv4() == v4)
            .map(ToString::to_string)
            .collect();
        if !hosts.is_empty() {
            let _ = writeln!(
                body,
                "\tset nodes_{suffix} {{ type {kind}; flags interval; auto-merge; elements = {{ {} }} }}",
                hosts.join(", ")
            );
            let _ = writeln!(
                rules,
                "\t\tct state new {family} saddr @workloads_{suffix} {family} daddr @nodes_{suffix} meta l4proto {{ tcp, udp }} th dport != {{ {ports} }} counter reject with icmpx type admin-prohibited"
            );
        }
    }
    let _ = writeln!(
        body,
        "\tchain forward {{\n\t\ttype filter hook forward priority {HOOK_PRIORITY}; policy accept;\n{rules}\t}}"
    );
    format!(
        "# Generated by temps-workload-netpol. Do not edit: the next apply replaces it.\n\
         table inet {table}\ndelete table inet {table}\ntable inet {table} {{\n{body}}}\n"
    )
}

/// Removes the table, and with it every policy it enforced. Idempotent:
/// declaring the table first makes the delete succeed when it is absent.
pub fn removal_ruleset(table: &TableName) -> String {
    format!("table inet {table}\ndelete table inet {table}\n")
}

/// A direction only needs a chain when something can be rejected in it.
fn needs_chain(policy: &WorkloadPolicy, direction: Direction) -> bool {
    let profile = policy.profile(direction);
    profile != Profile::Open
        || policy
            .rules
            .iter()
            .any(|r| r.direction == direction && r.action == Action::Deny)
}

fn set_base(workload: &WorkloadRef) -> String {
    format!("w_{}_{}", workload.kind.as_str(), workload.id)
}

fn chain_name(workload: &WorkloadRef, direction: Direction) -> String {
    let prefix = match direction {
        Direction::Egress => "egress",
        Direction::Ingress => "ingress",
    };
    format!("{prefix}_{}_{}", workload.kind.as_str(), workload.id)
}

fn write_sets(out: &mut String, workload: &WorkloadRef, identities: &IdentityMap) {
    let name = set_base(workload);
    let (v4, v6): (Vec<_>, Vec<_>) = identities.addresses(workload).partition(|a| a.is_ipv4());
    for (suffix, kind, addrs) in [("v4", "ipv4_addr", v4), ("v6", "ipv6_addr", v6)] {
        let _ = write!(out, "\tset {name}_{suffix} {{\n\t\ttype {kind};\n");
        if !addrs.is_empty() {
            let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
            let _ = writeln!(out, "\t\telements = {{ {} }}", list.join(", "));
        }
        let _ = writeln!(out, "\t}}");
    }
}

fn write_direction_chain(
    out: &mut String,
    policy: &WorkloadPolicy,
    direction: Direction,
    warnings: &mut Vec<String>,
) {
    let profile = policy.profile(direction);
    let mut rules: Vec<&Rule> = policy
        .rules
        .iter()
        .filter(|r| r.direction == direction)
        .collect();
    rules.sort_by_key(|r| (r.action == Action::Allow, r.id));

    let _ = writeln!(out, "\tchain {} {{", chain_name(&policy.subject, direction));
    let _ = writeln!(out, "\t\tct state established,related return");
    for rule in rules {
        let verdict = match (rule.action, profile) {
            (Action::Deny, _) => "counter reject with icmpx type admin-prohibited",
            (Action::Allow, Profile::None) => {
                warnings.push(format!(
                    "rule {} of {} allows {} traffic, but the {} profile is 'none': nothing passes",
                    rule.id,
                    policy.subject,
                    direction_word(direction),
                    direction_word(direction),
                ));
                continue;
            }
            (Action::Allow, Profile::Open) => {
                // Unreachable through needs_chain unless a deny shares the
                // chain; an allow under an open profile changes nothing.
                warnings.push(format!(
                    "rule {} of {} allows {} traffic, but the {} profile is 'open': it has no effect until the profile is 'restricted'",
                    rule.id,
                    policy.subject,
                    direction_word(direction),
                    direction_word(direction),
                ));
                continue;
            }
            (Action::Allow, Profile::Restricted) => "return",
        };
        for matcher in peer_matchers(rule, direction) {
            let l4 = l4_match(rule.protocol, rule.ports);
            let line = [matcher.as_str(), l4.as_str(), verdict]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(out, "\t\t{line}");
        }
    }
    if profile != Profile::Open {
        let _ = writeln!(out, "\t\tcounter reject with icmpx type admin-prohibited");
    }
    let _ = writeln!(out, "\t}}");
}

fn direction_word(direction: Direction) -> &'static str {
    match direction {
        Direction::Egress => "egress",
        Direction::Ingress => "ingress",
    }
}

/// The address matches for a rule's peer: the destination for egress, the
/// source for ingress. A workload peer matches both families.
fn peer_matchers(rule: &Rule, direction: Direction) -> Vec<String> {
    let field = match direction {
        Direction::Egress => "daddr",
        Direction::Ingress => "saddr",
    };
    match rule.peer {
        Peer::Workload(peer) => {
            let name = set_base(&peer);
            vec![
                format!("ip {field} @{name}_v4"),
                format!("ip6 {field} @{name}_v6"),
            ]
        }
        Peer::Cidr(cidr) if cidr.prefix() == 0 => {
            // Every address of one family: match the family alone.
            let family = if cidr.is_ipv4() { "ipv4" } else { "ipv6" };
            vec![format!("meta nfproto {family}")]
        }
        Peer::Cidr(cidr) => {
            let family = if cidr.is_ipv4() { "ip" } else { "ip6" };
            vec![format!("{family} {field} {cidr}")]
        }
    }
}

fn l4_match(protocol: L4Protocol, ports: Option<PortRange>) -> String {
    let range = ports.map(|p| {
        if p.start == p.end {
            p.start.to_string()
        } else {
            format!("{}-{}", p.start, p.end)
        }
    });
    match (protocol, range) {
        (L4Protocol::Tcp, Some(r)) => format!("tcp dport {r}"),
        (L4Protocol::Udp, Some(r)) => format!("udp dport {r}"),
        (L4Protocol::Any, Some(r)) => format!("meta l4proto {{ tcp, udp }} th dport {r}"),
        (L4Protocol::Tcp, None) => "meta l4proto tcp".to_string(),
        (L4Protocol::Udp, None) => "meta l4proto udp".to_string(),
        (L4Protocol::Any, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Cidr, Origin};
    use temps_workload::WorkloadKind;

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }

    fn service(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Service, id)
    }

    fn rule(id: i64, direction: Direction, peer: Peer, port: Option<u16>, action: Action) -> Rule {
        Rule {
            id,
            direction,
            peer,
            ports: port.map(PortRange::single),
            protocol: L4Protocol::Tcp,
            action,
            origin: Origin::Operator,
        }
    }

    fn identities() -> IdentityMap {
        IdentityMap::new()
            .with(sandbox(1), "172.20.0.5".parse().unwrap())
            .with(service(9), "172.20.0.9".parse().unwrap())
            .with(service(9), "fd00::9".parse().unwrap())
    }

    fn restricted_sandbox() -> WorkloadPolicy {
        WorkloadPolicy {
            subject: sandbox(1),
            egress: Profile::Restricted,
            ingress: Profile::Open,
            rules: vec![
                rule(
                    2,
                    Direction::Egress,
                    Peer::Workload(service(9)),
                    Some(5432),
                    Action::Allow,
                ),
                rule(
                    3,
                    Direction::Egress,
                    Peer::Cidr("10.0.0.0/8".parse().unwrap()),
                    None,
                    Action::Deny,
                ),
            ],
        }
    }

    #[test]
    fn workloads_cannot_open_published_ports_through_a_node_address() {
        let table = TableName::new("temps_netpol_node3").unwrap();
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        let workloads: BTreeSet<std::net::IpAddr> =
            [ip("10.20.30.5"), ip("fd00:20::5")].into_iter().collect();
        let hosts: Vec<Cidr> = vec![
            "10.20.1.4/32".parse().unwrap(),
            "100.64.0.0/10".parse().unwrap(),
        ];
        let ruleset = host_port_ruleset(&table, &workloads, &hosts);
        assert!(ruleset.contains("table inet temps_netpol_node3_hp {"));
        assert!(ruleset.contains("set workloads_v4 { type ipv4_addr; elements = { 10.20.30.5 } }"));
        assert!(ruleset.contains("elements = { 10.20.1.4/32, 100.64.0.0/10 }"));
        // Another node's host address, outside the public ingress.
        assert!(ruleset.contains(
            "ct state new ip saddr @workloads_v4 ip daddr @nodes_v4 meta l4proto { tcp, udp } th dport != { 80, 443 } counter reject"
        ));
        // This node's own published ports, recognised after DNAT.
        assert!(ruleset.contains(
            "ct state new ip saddr @workloads_v4 ct status dnat meta l4proto { tcp, udp } ct original proto-dst != { 80, 443 } counter reject"
        ));
        assert!(ruleset.contains("ip6 saddr @workloads_v6 ct status dnat"));
        assert!(
            !ruleset.contains("@nodes_v6"),
            "no IPv6 node address, no IPv6 node rule"
        );

        let none = host_port_ruleset(&table, &BTreeSet::new(), &hosts);
        assert_eq!(none, removal_ruleset(&host_port_table(&table)));
    }

    #[test]
    fn restricted_egress_matches_the_golden_ruleset() {
        let compiled = compile(
            &TableName::default_table(),
            &[restricted_sandbox()],
            &identities(),
        )
        .unwrap();
        // The fixture carries the repository's attribution header, which
        // the compiler does not emit.
        const HEADER: &str = "# SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n# SPDX-License-Identifier: MIT OR Apache-2.0\n\n";
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/restricted-egress.nft"
        );
        if std::env::var_os("TEMPS_UPDATE_GOLDEN").is_some() {
            std::fs::write(path, format!("{HEADER}{}", compiled.ruleset)).unwrap();
        }
        let golden = std::fs::read_to_string(path).unwrap();
        let golden = golden.strip_prefix(HEADER).unwrap_or(&golden);
        assert_eq!(compiled.ruleset, golden, "ruleset drifted from {path}");
        assert!(compiled.warnings.is_empty());
    }

    #[test]
    fn a_deny_is_placed_before_every_allow() {
        let compiled = compile(
            &TableName::default_table(),
            &[restricted_sandbox()],
            &identities(),
        )
        .unwrap();
        let deny = compiled.ruleset.find("ip daddr 10.0.0.0/8").unwrap();
        let allow = compiled
            .ruleset
            .find("@w_service_9_v4 tcp dport 5432 return")
            .unwrap();
        assert!(deny < allow, "{}", compiled.ruleset);
    }

    #[test]
    fn output_does_not_depend_on_input_order() {
        let mut a = restricted_sandbox();
        let b = WorkloadPolicy {
            subject: service(9),
            egress: Profile::Open,
            ingress: Profile::Restricted,
            rules: vec![rule(
                7,
                Direction::Ingress,
                Peer::Workload(sandbox(1)),
                Some(5432),
                Action::Allow,
            )],
        };
        let first = compile(
            &TableName::default_table(),
            &[a.clone(), b.clone()],
            &identities(),
        )
        .unwrap();
        a.rules.reverse();
        let second = compile(&TableName::default_table(), &[b, a], &identities()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn open_policies_without_rules_emit_no_chains() {
        let open = WorkloadPolicy {
            subject: sandbox(4),
            egress: Profile::Open,
            ingress: Profile::Open,
            rules: vec![],
        };
        let compiled = compile(&TableName::default_table(), &[open], &identities()).unwrap();
        assert!(
            !compiled.ruleset.contains("w_sandbox_4"),
            "{}",
            compiled.ruleset
        );
        assert!(compiled.ruleset.contains("hook forward priority -190"));
    }

    #[test]
    fn ingress_allows_under_an_open_profile_do_not_isolate() {
        let policy = WorkloadPolicy {
            subject: service(9),
            egress: Profile::Open,
            ingress: Profile::Open,
            rules: vec![rule(
                5,
                Direction::Ingress,
                Peer::Workload(sandbox(1)),
                Some(5432),
                Action::Allow,
            )],
        };
        let compiled = compile(&TableName::default_table(), &[policy], &identities()).unwrap();
        assert!(
            !compiled.ruleset.contains("ingress_service_9"),
            "{}",
            compiled.ruleset
        );
        assert!(!compiled.ruleset.contains("reject"));
    }

    #[test]
    fn a_none_profile_ignores_allows_and_says_so() {
        let mut policy = restricted_sandbox();
        policy.egress = Profile::None;
        let compiled = compile(&TableName::default_table(), &[policy], &identities()).unwrap();
        assert!(
            !compiled.ruleset.contains("tcp dport 5432 return"),
            "{}",
            compiled.ruleset
        );
        assert_eq!(compiled.warnings.len(), 1);
        assert!(
            compiled.warnings[0].contains("rule 2 of sandbox/1"),
            "{:?}",
            compiled.warnings
        );
    }

    #[test]
    fn ingress_matches_the_source_and_egress_the_destination() {
        let policy = WorkloadPolicy {
            subject: service(9),
            egress: Profile::Restricted,
            ingress: Profile::Restricted,
            rules: vec![
                rule(
                    1,
                    Direction::Ingress,
                    Peer::Workload(sandbox(1)),
                    Some(5432),
                    Action::Allow,
                ),
                rule(
                    2,
                    Direction::Egress,
                    Peer::Cidr("192.0.2.0/24".parse().unwrap()),
                    Some(443),
                    Action::Allow,
                ),
            ],
        };
        let compiled = compile(&TableName::default_table(), &[policy], &identities()).unwrap();
        assert!(compiled
            .ruleset
            .contains("ip saddr @w_sandbox_1_v4 tcp dport 5432 return"));
        assert!(compiled
            .ruleset
            .contains("ip daddr 192.0.2.0/24 tcp dport 443 return"));
        assert!(compiled
            .ruleset
            .contains("ip daddr @w_service_9_v4 jump ingress_service_9"));
        assert!(compiled
            .ruleset
            .contains("ip6 saddr @w_service_9_v6 jump egress_service_9"));
        assert!(compiled.ruleset.contains("elements = { fd00::9 }"));
    }

    #[test]
    fn protocols_and_ranges_render() {
        assert_eq!(
            l4_match(L4Protocol::Udp, Some(PortRange::single(53))),
            "udp dport 53"
        );
        assert_eq!(
            l4_match(
                L4Protocol::Any,
                Some(PortRange {
                    start: 8000,
                    end: 8010
                })
            ),
            "meta l4proto { tcp, udp } th dport 8000-8010"
        );
        assert_eq!(l4_match(L4Protocol::Tcp, None), "meta l4proto tcp");
        assert_eq!(l4_match(L4Protocol::Any, None), "");
        let everything = rule(
            1,
            Direction::Egress,
            Peer::Cidr("0.0.0.0/0".parse::<Cidr>().unwrap()),
            None,
            Action::Allow,
        );
        assert_eq!(
            peer_matchers(&everything, Direction::Egress),
            ["meta nfproto ipv4"]
        );
    }

    #[test]
    fn invalid_input_is_refused_with_context() {
        let table = TableName::default_table();
        let mut bad_ports = restricted_sandbox();
        bad_ports.rules[0].ports = Some(PortRange { start: 10, end: 1 });
        assert_eq!(
            compile(&table, &[bad_ports], &identities()),
            Err(CompileError::InvalidPorts {
                subject: sandbox(1),
                rule_id: 2,
                start: 10,
                end: 1
            })
        );
        assert_eq!(
            compile(
                &table,
                &[restricted_sandbox(), restricted_sandbox()],
                &identities()
            ),
            Err(CompileError::DuplicateSubject {
                subject: sandbox(1)
            })
        );
        let mut self_peer = restricted_sandbox();
        self_peer.rules[0].peer = Peer::Workload(sandbox(1));
        assert_eq!(
            compile(&table, &[self_peer], &identities()),
            Err(CompileError::SelfPeer {
                subject: sandbox(1),
                rule_id: 2
            })
        );
        assert!(TableName::new("Temps").is_err());
        assert!(TableName::new("t-1").is_err());
        assert!(TableName::new("a".repeat(33)).is_err());
        assert!(TableName::new("temps_netpol_e2e_42").is_ok());
    }

    #[test]
    fn removal_is_idempotent_by_construction() {
        assert_eq!(
            removal_ruleset(&TableName::default_table()),
            "table inet temps_netpol\ndelete table inet temps_netpol\n"
        );
    }
}
