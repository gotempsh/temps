// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The current addresses of each workload on this node's view.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use temps_workload::WorkloadRef;

/// Workload identity → the addresses of its instances. The reconciler
/// rebuilds it from instance status; the compiler turns each entry into
/// an nftables set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityMap {
    addresses: BTreeMap<WorkloadRef, BTreeSet<IpAddr>>,
}

impl IdentityMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, workload: WorkloadRef, addr: IpAddr) -> &mut Self {
        self.addresses.entry(workload).or_default().insert(addr);
        self
    }

    pub fn with(mut self, workload: WorkloadRef, addr: IpAddr) -> Self {
        self.insert(workload, addr);
        self
    }

    /// The addresses of `workload`, empty if it has no instance here.
    pub fn addresses(&self, workload: &WorkloadRef) -> impl Iterator<Item = IpAddr> + '_ {
        self.addresses.get(workload).into_iter().flatten().copied()
    }

    pub fn workloads(&self) -> impl Iterator<Item = &WorkloadRef> {
        self.addresses.keys()
    }

    /// The workload an address belongs to, if exactly one claims it.
    pub fn owner_of(&self, addr: IpAddr) -> Option<WorkloadRef> {
        let mut owners = self
            .addresses
            .iter()
            .filter(|(_, addrs)| addrs.contains(&addr))
            .map(|(w, _)| *w);
        let first = owners.next()?;
        owners.next().is_none().then_some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_workload::WorkloadKind;

    #[test]
    fn addresses_are_sorted_and_deduplicated() {
        let w = WorkloadRef::new(WorkloadKind::Sandbox, 1);
        let mut map = IdentityMap::new();
        map.insert(w, "172.20.0.9".parse().unwrap())
            .insert(w, "172.20.0.5".parse().unwrap())
            .insert(w, "172.20.0.9".parse().unwrap());
        let addrs: Vec<String> = map.addresses(&w).map(|a| a.to_string()).collect();
        assert_eq!(addrs, ["172.20.0.5", "172.20.0.9"]);
        assert_eq!(
            map.addresses(&WorkloadRef::new(WorkloadKind::Job, 1))
                .count(),
            0
        );
    }

    #[test]
    fn an_address_claimed_twice_has_no_owner() {
        let a = WorkloadRef::new(WorkloadKind::Sandbox, 1);
        let b = WorkloadRef::new(WorkloadKind::Sandbox, 2);
        let ip: IpAddr = "172.20.0.5".parse().unwrap();
        let map = IdentityMap::new().with(a, ip);
        assert_eq!(map.owner_of(ip), Some(a));
        let map = map.with(b, ip);
        assert_eq!(map.owner_of(ip), None);
    }
}
