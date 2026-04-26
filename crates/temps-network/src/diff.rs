//! Pure-logic peer / route diff.
//!
//! Reconciliation is idempotent: given the current kernel state and the
//! desired state, the diff routines compute the minimum set of add/remove
//! operations. This module is platform-independent and exhaustively unit-
//! tested without touching the kernel — Linux-specific transports apply
//! these diffs in `crate::linux`.

use crate::config::Peer;
use ipnet::Ipv4Net;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use uuid::Uuid;

/// Set of FDB / route mutations needed to move from `current` to `desired`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDiff {
    /// Peers that exist in `desired` but not `current` — must be added.
    pub to_add: Vec<Peer>,
    /// Underlay addresses present in `current` but absent in `desired` —
    /// their FDB entries must be removed. We key by underlay because the
    /// kernel's FDB doesn't carry node-id metadata.
    pub fdb_to_remove: Vec<IpAddr>,
    /// Peers whose compute_cidr or underlay changed and therefore need a
    /// remove-then-add. Kept separate so callers can apply the order
    /// safely.
    pub to_replace: Vec<(Peer, Peer)>,
}

impl PeerDiff {
    /// Compute the diff between `current` and `desired`.
    ///
    /// `current` and `desired` are unordered. Both lists must be free of
    /// duplicate `node_id`s — call sites should already have validated this
    /// via [`crate::NetworkConfig::validate_with`].
    pub fn compute(current: &[Peer], desired: &[Peer]) -> Self {
        let by_id_current: HashMap<Uuid, &Peer> = current.iter().map(|p| (p.node_id, p)).collect();
        let by_id_desired: HashMap<Uuid, &Peer> = desired.iter().map(|p| (p.node_id, p)).collect();

        let mut to_add: Vec<Peer> = Vec::new();
        let mut fdb_to_remove: Vec<IpAddr> = Vec::new();
        let mut to_replace: Vec<(Peer, Peer)> = Vec::new();

        for (id, want) in &by_id_desired {
            match by_id_current.get(id) {
                None => to_add.push((*want).clone()),
                Some(have) if have != want => {
                    to_replace.push(((**have).clone(), (*want).clone()));
                }
                Some(_) => {}
            }
        }

        for (id, have) in &by_id_current {
            if !by_id_desired.contains_key(id) {
                fdb_to_remove.push(have.underlay_address);
            }
        }

        // Stable order so tests aren't flaky on HashMap iteration order.
        to_add.sort_by_key(|p| p.node_id);
        fdb_to_remove.sort();
        to_replace.sort_by_key(|(_, want)| want.node_id);

        Self {
            to_add,
            fdb_to_remove,
            to_replace,
        }
    }

    /// True when no kernel changes are needed.
    pub fn is_noop(&self) -> bool {
        self.to_add.is_empty() && self.fdb_to_remove.is_empty() && self.to_replace.is_empty()
    }
}

/// Set of route mutations.
///
/// Routes are identified by destination CIDR; the gateway / device is implied
/// by the transport. When a peer's CIDR changes, the route is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDiff {
    pub to_add: Vec<Ipv4Net>,
    pub to_remove: Vec<Ipv4Net>,
}

impl RouteDiff {
    /// Compute the route diff implied by a peer diff.
    pub fn from_peer_diff(diff: &PeerDiff) -> Self {
        let mut to_add: HashSet<Ipv4Net> = HashSet::new();
        let mut to_remove: HashSet<Ipv4Net> = HashSet::new();

        for p in &diff.to_add {
            to_add.insert(p.compute_cidr);
        }
        for (have, want) in &diff.to_replace {
            if have.compute_cidr != want.compute_cidr {
                to_remove.insert(have.compute_cidr);
                to_add.insert(want.compute_cidr);
            }
        }
        // FDB-only removals (peer gone) — we need the CIDR as well. The peer
        // diff doesn't carry that for `fdb_to_remove`, so the orchestrator
        // passes the previous full peer set to [`Self::compute`] when the
        // CIDR mapping matters.
        let mut to_add: Vec<_> = to_add.into_iter().collect();
        let mut to_remove: Vec<_> = to_remove.into_iter().collect();
        to_add.sort();
        to_remove.sort();

        Self { to_add, to_remove }
    }

    /// Compute a full route diff between two peer lists. Use this when you
    /// have both lists handy — it's simpler than threading a partial
    /// `fdb_to_remove` through.
    pub fn compute(current: &[Peer], desired: &[Peer]) -> Self {
        let cur: HashSet<Ipv4Net> = current.iter().map(|p| p.compute_cidr).collect();
        let want: HashSet<Ipv4Net> = desired.iter().map(|p| p.compute_cidr).collect();

        let mut to_add: Vec<_> = want.difference(&cur).copied().collect();
        let mut to_remove: Vec<_> = cur.difference(&want).copied().collect();
        to_add.sort();
        to_remove.sort();

        Self { to_add, to_remove }
    }

    pub fn is_noop(&self) -> bool {
        self.to_add.is_empty() && self.to_remove.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    fn peer(id: u128, cidr: &str, underlay: &str) -> Peer {
        Peer {
            node_id: Uuid::from_u128(id),
            compute_cidr: Ipv4Net::from_str(cidr).unwrap(),
            underlay_address: IpAddr::V4(Ipv4Addr::from_str(underlay).unwrap()),
        }
    }

    #[test]
    fn empty_to_empty_is_noop() {
        let d = PeerDiff::compute(&[], &[]);
        assert!(d.is_noop());
    }

    #[test]
    fn add_single_peer() {
        let p = peer(1, "172.20.1.0/24", "10.0.0.1");
        let d = PeerDiff::compute(&[], std::slice::from_ref(&p));
        assert_eq!(d.to_add, vec![p]);
        assert!(d.fdb_to_remove.is_empty());
        assert!(d.to_replace.is_empty());
    }

    #[test]
    fn remove_single_peer() {
        let p = peer(1, "172.20.1.0/24", "10.0.0.1");
        let d = PeerDiff::compute(std::slice::from_ref(&p), &[]);
        assert!(d.to_add.is_empty());
        assert_eq!(d.fdb_to_remove, vec![p.underlay_address]);
    }

    #[test]
    fn replace_when_underlay_changes() {
        let have = peer(1, "172.20.1.0/24", "10.0.0.1");
        let want = peer(1, "172.20.1.0/24", "10.0.0.99");
        let d = PeerDiff::compute(std::slice::from_ref(&have), std::slice::from_ref(&want));
        assert!(d.to_add.is_empty());
        assert!(d.fdb_to_remove.is_empty());
        assert_eq!(d.to_replace, vec![(have, want)]);
    }

    #[test]
    fn replace_when_cidr_changes() {
        let have = peer(1, "172.20.1.0/24", "10.0.0.1");
        let want = peer(1, "172.20.99.0/24", "10.0.0.1");
        let d = PeerDiff::compute(std::slice::from_ref(&have), std::slice::from_ref(&want));
        assert_eq!(d.to_replace, vec![(have, want)]);
    }

    #[test]
    fn unchanged_peers_are_skipped() {
        let p = peer(1, "172.20.1.0/24", "10.0.0.1");
        let d = PeerDiff::compute(std::slice::from_ref(&p), std::slice::from_ref(&p));
        assert!(d.is_noop());
    }

    #[test]
    fn order_independent() {
        let a = peer(1, "172.20.1.0/24", "10.0.0.1");
        let b = peer(2, "172.20.2.0/24", "10.0.0.2");
        let d1 = PeerDiff::compute(&[], &[a.clone(), b.clone()]);
        let d2 = PeerDiff::compute(&[], &[b.clone(), a.clone()]);
        assert_eq!(d1, d2);
    }

    #[test]
    fn route_diff_from_peer_lists() {
        let a = peer(1, "172.20.1.0/24", "10.0.0.1");
        let b = peer(2, "172.20.2.0/24", "10.0.0.2");
        let c = peer(3, "172.20.3.0/24", "10.0.0.3");
        // current: a,b — desired: b,c → add c, remove a
        let d = RouteDiff::compute(&[a.clone(), b.clone()], &[b, c.clone()]);
        assert_eq!(d.to_add, vec![c.compute_cidr]);
        assert_eq!(d.to_remove, vec![a.compute_cidr]);
    }
}
