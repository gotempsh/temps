// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Anti-spoofing: each instance's source addresses are bound to the bridge
//! port its traffic enters through (ADR-055 §9).
//!
//! Every rule in [`crate::nft`] is keyed by source address. Without this
//! binding, a workload that can set its own address (one with `NET_ADMIN`,
//! or a microVM with its own kernel) could send with another workload's
//! address and be judged by that workload's rules. The binding is checked in
//! the `bridge` family at `prerouting`, before the frame is bridged or
//! routed, so a spoofed packet never reaches the policy chains, the egress
//! proxy or the resolver:
//!
//! ```text
//! table bridge temps_netpol {
//!   set port_sandbox_1_v4 { type ipv4_addr; elements = { 172.20.0.5 } }
//!   chain prerouting {
//!     type filter hook prerouting priority -300; policy accept;
//!     iifname "veth1a2b3c4" ether type ip ip saddr != @port_sandbox_1_v4 counter drop
//!     iifname "veth1a2b3c4" ether type arp arp saddr ip != @port_sandbox_1_v4 counter drop
//!     iifname "veth1a2b3c4" ether type ip6 ip6 saddr != @port_sandbox_1_v6 counter drop
//!   }
//! }
//! ```
//!
//! IPv6 link-local (`fe80::/10`) and the unspecified address stay allowed so
//! neighbour discovery keeps working; neither ever appears in a policy set.
//! A port with no bound address in a family drops that family entirely.
//!
//! The port is the host-side interface of the instance's veth pair, found by
//! [`crate::ports`]. It is resolved again whenever the instance restarts,
//! because a restart creates a new pair.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use temps_workload::WorkloadRef;

use crate::nft::TableName;

/// Priority of the anti-spoofing chain: the earliest bridge hook, before
/// connection tracking and before any policy.
pub const BINDING_PRIORITY: i32 = -300;

/// One instance's bridge port and the addresses it may send from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortBinding {
    pub workload: WorkloadRef,
    /// Host-side interface name (`vethXXXXXXX`).
    pub port: String,
    pub v4: BTreeSet<Ipv4Addr>,
    pub v6: BTreeSet<Ipv6Addr>,
}

impl PortBinding {
    pub fn new(workload: WorkloadRef, port: impl Into<String>) -> Self {
        Self {
            workload,
            port: port.into(),
            v4: BTreeSet::new(),
            v6: BTreeSet::new(),
        }
    }

    pub fn with(mut self, addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(v4) => {
                self.v4.insert(v4);
            }
            IpAddr::V6(v6) => {
                self.v6.insert(v6);
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error(
        "port '{port}' of {workload} is not a valid interface name: expected 1-15 characters of [A-Za-z0-9_.-]"
    )]
    InvalidPort { workload: WorkloadRef, port: String },
    #[error("port '{port}' is bound to both {first} and {second}; a port belongs to one instance")]
    SharedPort {
        port: String,
        first: WorkloadRef,
        second: WorkloadRef,
    },
}

/// Linux interface names: at most 15 bytes. Restricting the alphabet also
/// keeps the name from ever closing the quoted string in the ruleset.
fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.len() <= 15
        && port
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The `table bridge <table>` part of a node ruleset: replaced in the same
/// transaction as the `inet` table. Deterministic for a given input.
pub fn compile_bindings(
    table: &TableName,
    bindings: &[PortBinding],
) -> Result<String, BindingError> {
    let mut sorted: Vec<&PortBinding> = bindings.iter().collect();
    sorted.sort();
    let mut seen: std::collections::BTreeMap<&str, WorkloadRef> = Default::default();
    for binding in &sorted {
        if !valid_port(&binding.port) {
            return Err(BindingError::InvalidPort {
                workload: binding.workload,
                port: binding.port.clone(),
            });
        }
        if let Some(first) = seen.insert(&binding.port, binding.workload) {
            if first != binding.workload {
                return Err(BindingError::SharedPort {
                    port: binding.port.clone(),
                    first,
                    second: binding.workload,
                });
            }
        }
    }

    let mut sets = String::new();
    let mut rules = String::new();
    for (index, binding) in sorted.iter().enumerate() {
        let base = format!(
            "port_{}_{}_{index}",
            binding.workload.kind.as_str(),
            binding.workload.id
        );
        let v4: Vec<String> = binding.v4.iter().map(|a| a.to_string()).collect();
        let mut v6: Vec<String> = binding.v6.iter().map(|a| a.to_string()).collect();
        let _ = write!(sets, "\tset {base}_v4 {{\n\t\ttype ipv4_addr;\n");
        if !v4.is_empty() {
            let _ = writeln!(sets, "\t\telements = {{ {} }}", v4.join(", "));
        }
        let _ = writeln!(sets, "\t}}");
        // Neighbour discovery and duplicate address detection.
        v6.push("::".to_string());
        v6.push("fe80::/10".to_string());
        let _ = writeln!(
            sets,
            "\tset {base}_v6 {{\n\t\ttype ipv6_addr;\n\t\tflags interval;\n\t\telements = {{ {} }}\n\t}}",
            v6.join(", ")
        );
        let port = &binding.port;
        let _ = writeln!(
            rules,
            "\t\tiifname \"{port}\" ether type ip ip saddr != @{base}_v4 counter drop"
        );
        let _ = writeln!(
            rules,
            "\t\tiifname \"{port}\" ether type arp arp saddr ip != @{base}_v4 counter drop"
        );
        let _ = writeln!(
            rules,
            "\t\tiifname \"{port}\" ether type ip6 ip6 saddr != @{base}_v6 counter drop"
        );
    }

    let mut out = String::new();
    let _ = writeln!(out, "table bridge {table}");
    let _ = writeln!(out, "delete table bridge {table}");
    let _ = write!(
        out,
        "table bridge {table} {{\n{sets}\tchain prerouting {{\n\t\ttype filter hook prerouting priority {BINDING_PRIORITY}; policy accept;\n{rules}\t}}\n}}\n"
    );
    Ok(out)
}

/// Removes both tables a node ruleset installs. Idempotent.
pub fn node_removal_ruleset(table: &TableName) -> String {
    format!(
        "table inet {table}\ndelete table inet {table}\ntable bridge {table}\ndelete table bridge {table}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;

    fn sandbox(id: i64) -> WorkloadRef {
        WorkloadRef::new(WorkloadKind::Sandbox, id)
    }

    #[test]
    fn a_binding_drops_every_other_source_address() {
        let rules = compile_bindings(
            &TableName::default_table(),
            &[PortBinding::new(sandbox(1), "veth1a2b3c4").with("172.20.0.5".parse().unwrap())],
        )
        .unwrap();
        assert!(rules.starts_with(
            "table bridge temps_netpol\ndelete table bridge temps_netpol\ntable bridge temps_netpol {"
        ));
        assert!(rules.contains("elements = { 172.20.0.5 }"), "{rules}");
        assert!(
            rules.contains(
                "iifname \"veth1a2b3c4\" ether type ip ip saddr != @port_sandbox_1_0_v4 counter drop"
            ),
            "{rules}"
        );
        assert!(rules.contains("arp saddr ip != @port_sandbox_1_0_v4"));
        assert!(rules.contains("elements = { ::, fe80::/10 }"), "{rules}");
        assert!(rules.contains("hook prerouting priority -300"));
    }

    #[test]
    fn output_is_deterministic() {
        let a = PortBinding::new(sandbox(1), "veth1").with("10.0.0.2".parse().unwrap());
        let b = PortBinding::new(sandbox(2), "veth2").with("10.0.0.3".parse().unwrap());
        let table = TableName::default_table();
        assert_eq!(
            compile_bindings(&table, &[a.clone(), b.clone()]).unwrap(),
            compile_bindings(&table, &[b, a]).unwrap()
        );
    }

    #[test]
    fn hostile_port_names_and_shared_ports_are_refused() {
        let table = TableName::default_table();
        let bad = PortBinding::new(sandbox(1), "veth\" accept #");
        assert!(matches!(
            compile_bindings(&table, &[bad]),
            Err(BindingError::InvalidPort { .. })
        ));
        assert!(matches!(
            compile_bindings(&table, &[PortBinding::new(sandbox(1), "a".repeat(16))]),
            Err(BindingError::InvalidPort { .. })
        ));
        let shared = [
            PortBinding::new(sandbox(1), "veth1"),
            PortBinding::new(sandbox(2), "veth1"),
        ];
        assert_eq!(
            compile_bindings(&table, &shared),
            Err(BindingError::SharedPort {
                port: "veth1".into(),
                first: sandbox(1),
                second: sandbox(2)
            })
        );
    }

    #[test]
    fn no_bindings_still_produce_a_valid_empty_table() {
        let rules = compile_bindings(&TableName::default_table(), &[]).unwrap();
        assert!(rules.contains("chain prerouting"));
        assert!(!rules.contains("iifname"));
    }

    #[test]
    fn removal_covers_both_families() {
        let text = node_removal_ruleset(&TableName::default_table());
        assert!(text.contains("delete table inet temps_netpol"));
        assert!(text.contains("delete table bridge temps_netpol"));
    }
}
