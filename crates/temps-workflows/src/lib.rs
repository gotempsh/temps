// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable workflows orchestrated by the control plane (ADR-103).
//!
//! Temps keeps the state of every run (its status, lease, wait and the journal
//! of completed steps) and pushes invocations to the user's own deployed
//! service, which replays the journal through the SDK and runs the next step.
//! Nothing here depends on the deployments, proxy, analytics or error-tracking
//! crates: the executors are found through [`temps_core::ExecutorEndpoints`],
//! metrics leave through [`temps_core::ProjectMetricsSink`] and large payloads
//! through [`temps_core::PayloadBlobStore`].

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod e2e_docker_tests;
pub mod engine;
pub mod error;
pub mod executor;
pub mod handlers;
pub mod manifest;
pub mod metrics;
pub mod payload;
pub mod plugin;
pub mod protocol;
pub mod scheduler;
pub mod service;
pub mod store;
#[cfg(test)]
pub(crate) mod test_support;
pub mod token;
pub mod types;

pub use engine::WorkflowEngine;
pub use error::{WorkflowError, WorkflowResult};
pub use plugin::WorkflowsPlugin;
pub use scheduler::{SchedulerConfig, WorkflowScheduler};
pub use service::WorkflowService;
pub use store::{PgWorkflowStore, WorkflowStore};
