// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workload plane (ADR-055 §2, §4, §18).
//!
//! A product submits a [`temps_workload::WorkloadSpec`] through
//! [`WorkloadPlane::apply`] and reads the status the plane reports. The plane
//! owns everything between the spec and the runtime:
//!
//! - **Admission** ([`admission`]): one gate, in the ADR §4 order: defaults
//!   for the kind, the security profile (chosen together with the ADR-045
//!   socket grant), image allow-lists, quotas, validation. A refusal is a
//!   typed [`AdmissionError`] naming the rule and the workload, and the plane
//!   audit-logs it.
//! - **Objects** (`workloads`, `workload_instances`, `workload_events`): the
//!   only crate of the plane that touches the database.
//! - **Conditions and events** ([`events`]): status is written only when it
//!   changes; repeated events increment a count; at most 100 per workload and
//!   7 days are kept.
//! - **The reconciler** ([`reconciler`]): level-triggered and idempotent. A
//!   pass compares desired with actual state through the
//!   [`temps_workload::RuntimeDriver`] and fixes the difference, so a crash
//!   heals on the next pass. Passes run on change (an in-process notify on
//!   apply and on instance exit), at startup, and on a bounded slow resync,
//!   never as a hot table scan.
//!
//! Phase 2 reconciles `RunToCompletion` workloads on the control plane
//! (node 0), with Kubernetes Job semantics: a maximum run time, retries up to
//! a limit, a time-to-live for finished runs, and cleanup of instances the
//! plane created but no longer tracks.

pub mod admission;
pub mod audit;
pub mod env;
pub mod error;
pub mod events;
pub mod inventory;
pub mod plane;
pub mod reconciler;
pub mod scheduler;

#[cfg(test)]
mod testing;

pub use admission::{
    admit, AdmissionInput, AdmissionPolicy, AdmissionRule, ImageAllowList, Quotas,
    DEFAULT_JOB_TIMEOUT_SECS, DEFAULT_JOB_TTL_SECS,
};
pub use env::{EnvSource, NoEnvSource};
pub use error::{AdmissionError, PlaneError};
pub use plane::{
    ApplyRequest, Finalizer, NodeDriver, Owner, PlaneConfig, ReconcilerHandle, WorkloadPlane,
    WorkloadView,
};
pub use scheduler::{
    explain, recommend_pinning, ExplainContext, Explanation, NodeExclusion, NodeFacts,
    NodeRecommendation, NodeRole, NodeUsage, PinningRecommendation, Placement, Reservation,
    ScheduleError, ScheduleRequest, SchedulerConfig, SocketGate, UnifiedScheduler,
};

/// Labels the plane adds to every instance it creates, next to
/// [`temps_workload::labels`].
pub mod labels {
    /// Which plane created the instance. Orphan cleanup only ever touches
    /// instances carrying this plane's value, so two installations (or two
    /// test runs) sharing a Docker daemon never remove each other's.
    pub const PLANE: &str = "sh.temps.plane";
    /// Always [`MANAGED_BY_VALUE`]: the instance was created by the plane,
    /// not by a product's own deployer.
    pub const MANAGED_BY: &str = "sh.temps.managed_by";
    pub const MANAGED_BY_VALUE: &str = "workload-plane";
    /// The attempt the instance runs (1, then one more per retry).
    pub const ATTEMPT: &str = "sh.temps.attempt";

    /// Labels a product may not set: the plane owns them.
    pub const RESERVED: &[&str] = &[
        temps_workload::labels::KIND,
        temps_workload::labels::WORKLOAD_ID,
        temps_workload::labels::REVISION,
        temps_workload::labels::OWNER_ID,
        PLANE,
        MANAGED_BY,
        ATTEMPT,
    ];
}
