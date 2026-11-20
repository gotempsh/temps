// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the scheduler is fed: the nodes, from a product-supplied
//! [`NodeInventory`], and the placements this process decided but has not
//! started yet, from the [`InFlightLedger`].
//!
//! ADR-055 §5: "placements in flight count at their limit or the kind
//! default". A deployment that was just placed holds its share of the node
//! until its instance runs (and is then counted as committed by the
//! inventory), so two deployments placed a second apart do not both take the
//! last 512 MB. The ledger is per process: the control plane is the only
//! process that places, so that is every placement. It is a control-plane
//! structure (one entry per placement being started), never touched on a
//! request path.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::scheduler::{ExplainContext, NodeFacts, Reservation, SocketGate};

/// What a product knows about one placement beyond its spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementHints {
    /// The project the workload runs for (ADR-045 socket gate).
    pub project_slug: Option<String>,
    /// Platforms the artifact exists for; empty = no constraint.
    pub image_platforms: Vec<String>,
    /// Nodes already hosting the previous revision (spread avoids them).
    pub exclude_node_ids: Vec<i32>,
    /// Preferred nodes (a job: its owner's node).
    pub affinity_nodes: Vec<i32>,
    /// Keep the control plane out.
    pub exclude_control_plane: bool,
}

/// Where the scheduler's facts come from. Implemented by the product that
/// owns the `nodes` table (`temps-deployments`); the plane never reads it.
#[async_trait]
pub trait NodeInventory: Send + Sync {
    /// Every known node, node 0 first when the control plane may run
    /// workloads, workers by name. `grants_socket` answers for
    /// `project_slug`.
    async fn nodes(&self, project_slug: Option<&str>) -> Result<Vec<NodeFacts>, String>;

    /// The ADR-045 gate for `project_slug`, when the control plane declares
    /// it.
    fn socket_gate(&self, project_slug: Option<&str>) -> Option<SocketGate>;

    /// Linked services and cached images, for `explain`.
    async fn explain_context(
        &self,
        hints: &PlacementHints,
        image: Option<&str>,
    ) -> Result<ExplainContext, String>;
}

/// Reservations of placements decided by this process and not yet running.
#[derive(Debug, Default)]
pub struct InFlightLedger {
    entries: Mutex<BTreeMap<u64, (i32, Reservation)>>,
    next: Mutex<u64>,
}

impl InFlightLedger {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Hold `reservation` on each of `nodes` until the guard drops.
    pub fn hold(self: &Arc<Self>, nodes: &[i32], reservation: Reservation) -> InFlightGuard {
        let mut keys = Vec::with_capacity(nodes.len());
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        for node in nodes {
            *next += 1;
            entries.insert(*next, (*node, reservation));
            keys.push(*next);
        }
        InFlightGuard {
            ledger: Arc::clone(self),
            keys,
        }
    }

    /// What is in flight per node.
    pub fn by_node(&self) -> BTreeMap<i32, Reservation> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let mut totals: BTreeMap<i32, Reservation> = BTreeMap::new();
        for (node, reservation) in entries.values() {
            let total = totals.entry(*node).or_default();
            *total = *total + *reservation;
        }
        totals
    }

    /// Add what is in flight to each node's facts.
    pub fn apply(&self, nodes: &mut [NodeFacts]) {
        let totals = self.by_node();
        for node in nodes {
            if let Some(held) = totals.get(&node.id) {
                node.in_flight = node.in_flight + *held;
            }
        }
    }
}

/// Releases its reservations when dropped: when the instance runs (the
/// inventory then counts it as committed) or the placement is abandoned.
#[derive(Debug)]
pub struct InFlightGuard {
    ledger: Arc<InFlightLedger>,
    keys: Vec<u64>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut entries = self
            .ledger
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for key in &self.keys {
            entries.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_placement_counts_until_released() {
        let ledger = InFlightLedger::new();
        let need = Reservation {
            cpu_millis: 250,
            memory_mb: 512,
        };
        let first = ledger.hold(&[3, 3], need);
        let second = ledger.hold(&[4], need);
        let mut nodes = vec![NodeFacts::control_plane(None)];
        nodes[0].id = 3;
        ledger.apply(&mut nodes);
        assert_eq!(nodes[0].in_flight.memory_mb, 1024);
        drop(first);
        assert_eq!(ledger.by_node().get(&3), None);
        assert_eq!(ledger.by_node().get(&4), Some(&need));
        drop(second);
        assert!(ledger.by_node().is_empty());
    }
}
