// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workflow engine: invokes claimed runs, applies their outcomes,
//! delivers events and serves the executor's journal API (ADR-103 §3–§9).
//!
//! It holds no state of its own beyond caches: every decision is a fenced
//! write to the [`WorkflowStore`], so any number of control-plane processes
//! can run an engine against the same database.

mod events;
mod invoke;
mod journal;
mod outcome;
#[cfg(test)]
pub(crate) mod tests;

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use temps_core::ExecutorEndpoints;
use uuid::Uuid;

use crate::executor::ExecutorClient;
use crate::manifest::ManifestCache;
use crate::metrics::WorkflowMetrics;
use crate::payload::{PayloadCodec, PayloadScope, StoredPayload};
use crate::protocol::Manifest;
use crate::store::{RunRow, StepRow, WorkflowStore};
use crate::token::InvocationSigner;

/// Most journal entries sent in one invoke request or journal page.
pub const JOURNAL_PAGE_ENTRIES: u64 = 1_000;
/// Most decoded journal bytes sent in one invoke request or journal page;
/// the executor fetches the rest with `GET /workflow-runs/{id}/journal`.
pub const JOURNAL_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// Longest error message the executor may record, in bytes.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 64 * 1024;
/// How long a run waits before it is tried again when nothing could be
/// invoked (no healthy replica, no deployment).
pub const BLOCKED_RETRY_SECS: i64 = 30;

pub use events::{EventDelivery, EventSpec};
pub use journal::Authorized;

/// What [`WorkflowEngine::probe_executor`] found.
#[derive(Debug, Clone)]
pub struct ExecutorProbe {
    pub deployment_id: Option<i32>,
    pub replicas: usize,
    /// The manifest, or why it could not be read.
    pub manifest: Result<Arc<Manifest>, String>,
}

pub struct WorkflowEngine {
    store: Arc<dyn WorkflowStore>,
    codec: Arc<PayloadCodec>,
    signer: Arc<InvocationSigner>,
    endpoints: Arc<dyn ExecutorEndpoints>,
    client: ExecutorClient,
    manifests: ManifestCache,
    metrics: Arc<WorkflowMetrics>,
    next_endpoint: AtomicUsize,
}

impl WorkflowEngine {
    pub fn new(
        store: Arc<dyn WorkflowStore>,
        codec: Arc<PayloadCodec>,
        signer: Arc<InvocationSigner>,
        endpoints: Arc<dyn ExecutorEndpoints>,
        client: ExecutorClient,
        metrics: Arc<WorkflowMetrics>,
    ) -> Self {
        Self {
            store,
            codec,
            signer,
            endpoints,
            client,
            manifests: ManifestCache::new(),
            metrics,
            next_endpoint: AtomicUsize::new(0),
        }
    }

    /// The manifest last read for the environment, without reading it.
    pub fn cached_manifest(&self, environment_id: i32) -> Option<Arc<Manifest>> {
        self.manifests.cached(environment_id).and_then(Result::ok)
    }

    /// Forget the environment's manifest (its settings changed).
    pub fn invalidate_manifest(&self, environment_id: i32) {
        self.manifests.invalidate(environment_id);
    }

    /// Resolve the environment's replicas and read its manifest, for the
    /// overview's capability check.
    pub async fn probe_executor(
        &self,
        project_id: i32,
        environment_id: i32,
        host_service: Option<&str>,
    ) -> ExecutorProbe {
        let set = match self
            .endpoints
            .endpoints(project_id, environment_id, host_service)
            .await
        {
            Ok(set) => set,
            Err(error) => {
                return ExecutorProbe {
                    deployment_id: None,
                    replicas: 0,
                    manifest: Err(error.to_string()),
                }
            }
        };
        if set.endpoints.is_empty() {
            return ExecutorProbe {
                deployment_id: set.deployment_id,
                replicas: 0,
                manifest: Err(set
                    .unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "No healthy replica of the service is running".into())),
            };
        }
        let manifest = self
            .manifests
            .get(&self.client, &self.signer, project_id, environment_id, &set)
            .await;
        ExecutorProbe {
            deployment_id: set.deployment_id,
            replicas: set.endpoints.len(),
            manifest,
        }
    }

    pub(crate) fn scope(run: &RunRow) -> PayloadScope {
        PayloadScope {
            project_id: run.project_id,
            environment_id: run.environment_id,
            run_id: run.id,
        }
    }

    /// Events have no run: their payloads are bound to the nil run id and
    /// the event's name and key.
    pub(crate) fn event_scope(project_id: i32, environment_id: i32) -> PayloadScope {
        PayloadScope {
            project_id,
            environment_id,
            run_id: Uuid::nil(),
        }
    }
}

pub(crate) fn run_input(run: &RunRow) -> StoredPayload {
    StoredPayload {
        encrypted: run.input_encrypted.clone(),
        blob_key: run.input_blob_key.clone(),
        bytes: run.input_bytes,
    }
}

pub(crate) fn run_output(run: &RunRow) -> StoredPayload {
    StoredPayload {
        encrypted: run.output_encrypted.clone(),
        blob_key: run.output_blob_key.clone(),
        bytes: run.output_bytes,
    }
}

pub(crate) fn run_error(run: &RunRow) -> StoredPayload {
    StoredPayload {
        encrypted: run.error_encrypted.clone(),
        blob_key: None,
        bytes: 0,
    }
}

pub(crate) fn step_result(step: &StepRow) -> StoredPayload {
    StoredPayload {
        encrypted: step.result_encrypted.clone(),
        blob_key: step.result_blob_key.clone(),
        bytes: step.result_bytes,
    }
}

pub(crate) fn step_error(step: &StepRow) -> StoredPayload {
    StoredPayload {
        encrypted: step.error_encrypted.clone(),
        blob_key: None,
        bytes: 0,
    }
}
