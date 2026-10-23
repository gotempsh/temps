// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The node runtime (ADR-055 §7): one reconciler for the instances assigned
//! to a node.
//!
//! A worker reads its desired state from the control plane's stream
//! (`GET /internal/nodes/{id}/state?since=W`), applies it
//! ([`NodeRuntime::apply`]), and reconciles its runtime driver against it
//! ([`NodeRuntime::reconcile`]). What it observed goes back on the
//! heartbeat ([`NodeRuntime::pending_reports`]), only when it changed.
//!
//! The last received state is kept on disk ([`store::SnapshotStore`], mode
//! 0600) so instances can be restarted while the control plane is
//! unreachable. It holds specs and secret references only. Secret values
//! arrive in the per-instance create call and live in per-instance files
//! ([`store::SecretStore`], mode 0400). An instance that must be created
//! while the control plane is unreachable and whose secret file is gone is
//! not started: it reports `Running = False`, `SecretsUnavailable`, and is
//! retried when the stream reconnects.

pub mod runtime;
pub mod store;

pub use runtime::{
    ControlPlaneLink, LinkError, NodeRuntime, NodeRuntimeError, ReconcileOutcome, SyncError,
};
pub use store::{SecretStore, SnapshotStore, StoreError};
