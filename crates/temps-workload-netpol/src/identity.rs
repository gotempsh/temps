// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The current addresses of each workload on this node's view.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use temps_workload::WorkloadRef;

/// Workload identity → the addresses of its instances. The reconciler
/// rebuilds it from instance status; the compiler turns each entry into
/// an nftables set.
///
/// On the wire (a worker's policy snapshot) it is a list of
/// `{workload, addresses}`, since a workload is not a JSON object key.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(into = "Vec<IdentityEntry>", from = "Vec<IdentityEntry>")]
pub struct IdentityMap {
    addresses: BTreeMap<WorkloadRef, BTreeSet<IpAddr>>,
}

/// One workload's addresses in a serialized [`IdentityMap`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IdentityEntry {
    pub workload: WorkloadRef,
    pub addresses: Vec<IpAddr>,
}

impl From<IdentityMap> for Vec<IdentityEntry> {
    fn from(map: IdentityMap) -> Self {
        map.addresses
            .into_iter()
            .map(|(workload, addresses)| IdentityEntry {
                workload,
                addresses: addresses.into_iter().collect(),
            })
            .collect()
    }
}

impl From<Vec<IdentityEntry>> for IdentityMap {
    fn from(entries: Vec<IdentityEntry>) -> Self {
        let mut map = IdentityMap::new();
        for entry in entries {
            for addr in entry.addresses {
                map.insert(entry.workload, addr);
            }
        }
        map
    }
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

    /// Every address in the map, across workloads.
    pub fn address_count(&self) -> usize {
        self.addresses.values().map(BTreeSet::len).sum()
    }

    /// Keeps only the addresses `keep` accepts, dropping workloads left
    /// with none.
    pub fn retain_addresses(&mut self, keep: impl Fn(IpAddr) -> bool) {
        self.addresses.retain(|_, addrs| {
            addrs.retain(|addr| keep(*addr));
            !addrs.is_empty()
        });
    }

    /// Adds every address of `other`.
    pub fn extend(&mut self, other: &IdentityMap) {
        for (workload, addrs) in &other.addresses {
            self.addresses
                .entry(*workload)
                .or_default()
                .extend(addrs.iter().copied());
        }
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

    #[test]
    fn retaining_drops_addresses_and_emptied_workloads() {
        let a = WorkloadRef::new(WorkloadKind::Deployment, 1);
        let b = WorkloadRef::new(WorkloadKind::Deployment, 2);
        let mut map = IdentityMap::new()
            .with(a, "10.202.0.2".parse().unwrap())
            .with(a, "172.18.0.3".parse().unwrap())
            .with(b, "172.18.0.4".parse().unwrap());
        map.retain_addresses(|addr| addr.to_string().starts_with("10.202."));
        assert_eq!(map.workloads().copied().collect::<Vec<_>>(), [a]);
        assert_eq!(map.address_count(), 1);

        let mut merged = IdentityMap::new().with(b, "10.202.1.2".parse().unwrap());
        merged.extend(&map);
        assert_eq!(merged.address_count(), 2);
        assert_eq!(merged.owner_of("10.202.0.2".parse().unwrap()), Some(a));
    }
}
