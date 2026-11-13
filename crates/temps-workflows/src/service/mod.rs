// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The workflows service: the operations behind the management API and the
//! app-facing API (ADR-103 §10, §11). Every method is scoped to a project and
//! environment and checks that the environment belongs to the project, and
//! that a run belongs to the environment, before anything else.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::engine::{
    run_error, run_input, run_output, step_error, step_result, EventDelivery, EventSpec,
    WorkflowEngine,
};
use crate::error::{WorkflowError, WorkflowResult};
use crate::metrics::WorkflowMetrics;
use crate::payload::{PayloadCodec, PayloadField};
use crate::store::{
    CredentialRow, NewRun, RunFilter, RunRow, StartOutcome, Starter, StepRow, WorkflowStore,
};
use crate::token::{mint_credential, sha256_hex, CREDENTIAL_PREFIX};
use crate::types::{validate_key, validate_name, RunStatus, StepStatus, WorkflowSettings};

pub const DEFAULT_PAGE_SIZE: u64 = 20;
pub const MAX_PAGE_SIZE: u64 = 100;
/// Longest workflow name.
pub const MAX_WORKFLOW_NAME: usize = 128;
/// Where the console fixes a missing executor.
pub const EXECUTOR_SETUP_PATH: &str = "/docs/workflows/getting-started";

/// `(page, page_size)` with defaults and bounds applied.
pub fn page_bounds(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
    (
        page.unwrap_or(1).max(1),
        page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE),
    )
}

/// Whether the environment can run workflows, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorStatus {
    pub configured: bool,
    pub reason: Option<String>,
    pub setup_path: Option<String>,
    pub deployment_id: Option<i32>,
    pub replicas: usize,
    pub sdk: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowInfo {
    pub name: String,
    pub version: Option<String>,
    pub concurrency: Option<u32>,
    /// Defined by the current deployment's manifest.
    pub defined: bool,
    pub active_runs: i64,
    pub completed_runs: i64,
    pub failed_runs: i64,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub executor: ExecutorStatus,
    pub credential: Option<CredentialRow>,
    pub workflows: Vec<WorkflowInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartRun {
    pub workflow: String,
    pub input: Option<Value>,
    pub idempotency_key: Option<String>,
}

/// A payload an operator can reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealField {
    Input,
    Output,
    Error,
    StepResult(i32),
    StepError(i32),
}

impl RevealField {
    pub fn label(self) -> String {
        match self {
            RevealField::Input => "input".into(),
            RevealField::Output => "output".into(),
            RevealField::Error => "error".into(),
            RevealField::StepResult(sequence) => format!("step {sequence} result"),
            RevealField::StepError(sequence) => format!("step {sequence} error"),
        }
    }
}

/// A freshly rotated credential: `token` is shown once and never stored.
pub struct IssuedCredential {
    pub credential: CredentialRow,
    pub token: String,
}

impl std::fmt::Debug for IssuedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedCredential")
            .field("credential_id", &self.credential.id)
            .field("prefix", &self.credential.token_prefix)
            .finish_non_exhaustive()
    }
}

pub struct WorkflowService {
    store: Arc<dyn WorkflowStore>,
    engine: Arc<WorkflowEngine>,
    codec: Arc<PayloadCodec>,
    metrics: Arc<WorkflowMetrics>,
}

impl WorkflowService {
    pub fn new(
        store: Arc<dyn WorkflowStore>,
        engine: Arc<WorkflowEngine>,
        codec: Arc<PayloadCodec>,
        metrics: Arc<WorkflowMetrics>,
    ) -> Self {
        Self {
            store,
            engine,
            codec,
            metrics,
        }
    }

    async fn require_environment(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> WorkflowResult<()> {
        match self.store.environment_project(environment_id).await? {
            Some(owner) if owner == project_id => Ok(()),
            _ => Err(WorkflowError::EnvironmentNotFound {
                project_id,
                environment_id,
            }),
        }
    }

    /// The run, if it belongs to the environment (and the environment to the
    /// project).
    pub async fn get_run(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> WorkflowResult<RunRow> {
        self.require_environment(project_id, environment_id).await?;
        self.store
            .get_run(run_id)
            .await?
            .filter(|run| run.project_id == project_id && run.environment_id == environment_id)
            .ok_or(WorkflowError::RunNotFound {
                project_id,
                environment_id,
                run_id,
            })
    }

    // ── Overview ─────────────────────────────────────────────────────

    pub async fn overview(&self, project_id: i32, environment_id: i32) -> WorkflowResult<Overview> {
        self.require_environment(project_id, environment_id).await?;
        let settings = self.store.settings(environment_id).await?;
        let executor = self
            .executor_status(project_id, environment_id, &settings)
            .await;
        let credential = self.store.active_credential(environment_id).await?;
        let mut summaries = self.store.workflow_summaries(environment_id).await?;
        let manifest = self.engine.cached_manifest(environment_id);
        let mut workflows = Vec::new();
        if let Some(manifest) = &manifest {
            for workflow in &manifest.workflows {
                let summary = summaries.remove(&workflow.name);
                workflows.push(WorkflowInfo {
                    name: workflow.name.clone(),
                    version: workflow.version.clone(),
                    concurrency: workflow.concurrency,
                    defined: true,
                    active_runs: summary.as_ref().map_or(0, |s| s.active),
                    completed_runs: summary.as_ref().map_or(0, |s| s.completed),
                    failed_runs: summary.as_ref().map_or(0, |s| s.failed),
                    last_run_at: summary.and_then(|s| s.last_run_at),
                });
            }
        }
        // Workflows with runs that the deployment no longer defines.
        let mut rest: Vec<_> = summaries.into_values().collect();
        rest.sort_by(|a, b| a.workflow_name.cmp(&b.workflow_name));
        workflows.extend(rest.into_iter().map(|summary| WorkflowInfo {
            name: summary.workflow_name,
            version: None,
            concurrency: None,
            defined: false,
            active_runs: summary.active,
            completed_runs: summary.completed,
            failed_runs: summary.failed,
            last_run_at: summary.last_run_at,
        }));
        Ok(Overview {
            executor,
            credential,
            workflows,
        })
    }

    async fn executor_status(
        &self,
        project_id: i32,
        environment_id: i32,
        settings: &WorkflowSettings,
    ) -> ExecutorStatus {
        let probe = self
            .engine
            .probe_executor(project_id, environment_id, settings.host_service.as_deref())
            .await;
        match probe.manifest {
            Ok(manifest) => ExecutorStatus {
                configured: true,
                reason: None,
                setup_path: None,
                deployment_id: probe.deployment_id,
                replicas: probe.replicas,
                sdk: manifest.sdk.clone(),
            },
            Err(reason) => ExecutorStatus {
                configured: false,
                reason: Some(reason),
                setup_path: Some(EXECUTOR_SETUP_PATH.to_string()),
                deployment_id: probe.deployment_id,
                replicas: probe.replicas,
                sdk: None,
            },
        }
    }

    // ── Runs ─────────────────────────────────────────────────────────

    pub async fn list_runs(
        &self,
        project_id: i32,
        environment_id: i32,
        filter: &RunFilter,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<RunRow>, u64)> {
        self.require_environment(project_id, environment_id).await?;
        if let Some(name) = &filter.workflow_name {
            validate_name("workflow", name, MAX_WORKFLOW_NAME)?;
        }
        self.store
            .list_runs(environment_id, filter, page, page_size)
            .await
    }

    /// One page of the run's journal, in sequence order.
    pub async fn list_steps(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<StepRow>, u64)> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let from = i32::try_from((page - 1).saturating_mul(page_size)).unwrap_or(i32::MAX);
        let steps = self.store.journal(run_id, from, page_size).await?;
        Ok((steps, u64::try_from(run.step_count).unwrap_or(0)))
    }

    /// Decrypt one payload of a run for an operator. The caller audits it.
    pub async fn reveal(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
        field: RevealField,
    ) -> WorkflowResult<Option<Value>> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let settings = self.store.settings(environment_id).await?;
        let scope = WorkflowEngine::scope(&run);
        let (payload_field, stored) = match field {
            RevealField::Input => (PayloadField::RunInput, run_input(&run)),
            RevealField::Output => (PayloadField::RunOutput, run_output(&run)),
            RevealField::Error => (PayloadField::RunError, run_error(&run)),
            RevealField::StepResult(sequence) | RevealField::StepError(sequence) => {
                let step = self
                    .store
                    .step(run_id, sequence)
                    .await?
                    .ok_or(WorkflowError::StepNotFound { run_id, sequence })?;
                if matches!(field, RevealField::StepResult(_)) {
                    (PayloadField::StepResult(sequence), step_result(&step))
                } else {
                    (PayloadField::StepError(sequence), step_error(&step))
                }
            }
        };
        self.codec
            .open(&scope, &payload_field, &stored, &settings)
            .await
    }

    /// Start a run. Returns it and whether it was created (false: an earlier
    /// run with the same idempotency key).
    pub async fn start(
        &self,
        project_id: i32,
        environment_id: i32,
        request: StartRun,
        starter: Starter,
    ) -> WorkflowResult<(RunRow, bool)> {
        self.require_environment(project_id, environment_id).await?;
        validate_name("workflow", &request.workflow, MAX_WORKFLOW_NAME)?;
        if let Some(key) = &request.idempotency_key {
            validate_key("idempotency_key", key)?;
        }
        let settings = self.store.settings(environment_id).await?;
        let id = Uuid::new_v4();
        let scope = crate::payload::PayloadScope {
            project_id,
            environment_id,
            run_id: id,
        };
        let input = match &request.input {
            Some(input) => {
                self.codec
                    .seal(&scope, &PayloadField::RunInput, input, 0, &settings)
                    .await?
            }
            None => Default::default(),
        };
        let outcome = self
            .store
            .insert_run(
                NewRun {
                    id,
                    project_id,
                    environment_id,
                    workflow_name: request.workflow.clone(),
                    input: input.clone(),
                    idempotency_key: request.idempotency_key.clone(),
                    deployment_id: None,
                    starter,
                },
                i64::from(settings.max_active_runs),
            )
            .await;
        match outcome {
            Ok(StartOutcome::Created(run)) => {
                self.metrics
                    .run_started(project_id, environment_id, &run.workflow_name);
                tracing::info!(
                    run_id = %run.id,
                    project_id,
                    environment_id,
                    workflow = %run.workflow_name,
                    "workflow run started"
                );
                Ok((run, true))
            }
            Ok(StartOutcome::Existing(run)) => {
                self.codec.discard(project_id, &input).await;
                Ok((run, false))
            }
            Err(error) => {
                self.codec.discard(project_id, &input).await;
                Err(error)
            }
        }
    }

    pub async fn cancel(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> WorkflowResult<RunRow> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let cancelled =
            self.store
                .cancel_run(run_id)
                .await?
                .ok_or_else(|| WorkflowError::InvalidState {
                    run_id,
                    status: run.status.clone(),
                    operation: "cancel",
                })?;
        self.metrics
            .run_finished(project_id, environment_id, &run.workflow_name, "cancelled");
        tracing::info!(%run_id, project_id, environment_id, "workflow run cancelled");
        Ok(cancelled)
    }

    /// Run again from `sequence` (default: the entry that failed, or the end
    /// of the journal), dropping the journal from there on.
    pub async fn retry_from(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
        sequence: Option<i32>,
    ) -> WorkflowResult<RunRow> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let sequence = match sequence {
            Some(sequence) => sequence,
            None => {
                let last = run.step_count - 1;
                match self.store.step(run_id, last).await? {
                    Some(step) if step.status != StepStatus::Completed.as_str() => last,
                    _ => run.step_count,
                }
            }
        };
        if sequence < 0 || sequence > run.step_count {
            return Err(WorkflowError::Validation {
                field: "from_sequence",
                reason: format!(
                    "run {run_id} has {} journal entries; retry from 0 to {}",
                    run.step_count, run.step_count
                ),
            });
        }
        let retried = self
            .store
            .retry_from(run_id, sequence)
            .await?
            .ok_or_else(|| WorkflowError::InvalidState {
                run_id,
                status: run.status.clone(),
                operation:
                    "retry (only a completed, failed, cancelled or stuck run can be retried)",
            })?;
        tracing::info!(%run_id, project_id, environment_id, from_sequence = sequence, "workflow run retried");
        Ok(retried)
    }

    pub async fn resume(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> WorkflowResult<RunRow> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let resumed =
            self.store
                .resume_run(run_id)
                .await?
                .ok_or_else(|| WorkflowError::InvalidState {
                    run_id,
                    status: run.status.clone(),
                    operation:
                        "resume (only a stuck, retrying, sleeping or queued run can be resumed)",
                })?;
        tracing::info!(%run_id, project_id, environment_id, "workflow run resumed");
        Ok(resumed)
    }

    pub async fn delete(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> WorkflowResult<RunRow> {
        let run = self.get_run(project_id, environment_id, run_id).await?;
        let deleted =
            self.store
                .delete_run(run_id)
                .await?
                .ok_or_else(|| WorkflowError::InvalidState {
                    run_id,
                    status: run.status.clone(),
                    operation: "delete (cancel it first)",
                })?;
        if deleted.has_blob_payloads {
            self.codec
                .delete_run_blobs(&WorkflowEngine::scope(&deleted))
                .await;
        }
        tracing::info!(%run_id, project_id, environment_id, "workflow run deleted");
        Ok(deleted)
    }

    // ── Events ───────────────────────────────────────────────────────

    /// Store an event and deliver it to the runs waiting on it.
    pub async fn send_event(
        &self,
        project_id: i32,
        environment_id: i32,
        name: &str,
        key: Option<&str>,
        payload: Option<&Value>,
        source: &'static str,
    ) -> WorkflowResult<(i64, EventDelivery)> {
        self.require_environment(project_id, environment_id).await?;
        let event = self
            .engine
            .store_event(EventSpec {
                project_id,
                environment_id,
                name,
                key: key.unwrap_or_default(),
                payload,
                source,
                source_run_id: None,
            })
            .await?;
        let delivery = self.engine.deliver_event(&event).await?;
        Ok((event.id, delivery))
    }

    // ── Settings ─────────────────────────────────────────────────────

    pub async fn settings(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> WorkflowResult<WorkflowSettings> {
        self.require_environment(project_id, environment_id).await?;
        self.store.settings(environment_id).await
    }

    pub async fn update_settings(
        &self,
        project_id: i32,
        environment_id: i32,
        settings: WorkflowSettings,
    ) -> WorkflowResult<WorkflowSettings> {
        self.require_environment(project_id, environment_id).await?;
        settings.validate()?;
        let saved = self
            .store
            .upsert_settings(project_id, environment_id, &settings)
            .await?;
        // The host service may have changed: read the manifest afresh.
        self.engine.invalidate_manifest(environment_id);
        Ok(saved)
    }

    // ── Credentials ──────────────────────────────────────────────────

    pub async fn credential(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> WorkflowResult<Option<CredentialRow>> {
        self.require_environment(project_id, environment_id).await?;
        self.store.active_credential(environment_id).await
    }

    /// Issue a new credential, revoking the previous one.
    pub async fn rotate_credential(
        &self,
        project_id: i32,
        environment_id: i32,
        user_id: Option<i32>,
    ) -> WorkflowResult<IssuedCredential> {
        self.require_environment(project_id, environment_id).await?;
        let minted = mint_credential()?;
        let credential = self
            .store
            .rotate_credential(
                project_id,
                environment_id,
                &minted.hash,
                &minted.prefix,
                user_id,
            )
            .await?;
        tracing::info!(
            project_id,
            environment_id,
            credential_id = credential.id,
            "workflow credential issued"
        );
        Ok(IssuedCredential {
            credential,
            token: minted.token,
        })
    }

    /// Revoke the active credential. False when there was none.
    pub async fn revoke_credential(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> WorkflowResult<bool> {
        self.require_environment(project_id, environment_id).await?;
        self.store.revoke_credential(environment_id).await
    }

    /// The active credential a `twf_` token belongs to.
    pub async fn authenticate_credential(&self, token: &str) -> WorkflowResult<CredentialRow> {
        if !token.starts_with(CREDENTIAL_PREFIX) || token.len() > 128 {
            return Err(WorkflowError::InvalidCredential {
                reason: "not a workflow credential",
            });
        }
        self.store
            .credential_by_hash(&sha256_hex(token.as_bytes()))
            .await?
            .ok_or(WorkflowError::InvalidCredential {
                reason: "unknown or revoked credential",
            })
    }

    /// Whether a run's status lets an app read its output.
    pub fn is_finished(run: &RunRow) -> bool {
        RunStatus::parse(&run.status).is_some_and(RunStatus::is_terminal)
    }
}
