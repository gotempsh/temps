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

pub mod error;
pub mod payload;
pub mod protocol;
pub mod store;
pub mod token;
pub mod types;

pub use error::{WorkflowError, WorkflowResult};
pub use store::{PgWorkflowStore, WorkflowStore};
