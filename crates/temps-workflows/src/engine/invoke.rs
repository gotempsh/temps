// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One invocation of a claimed run (ADR-106 §3, §6): resolve a replica, read
//! the manifest, send the input and journal, apply the outcome.
//!
//! A run nothing could be invoked for (no deployment, no healthy replica) is
//! given back without spending an attempt and shows why; an invocation that
//! failed (unreachable, timed out, an HTTP error, a workflow the deployment
//! does not serve) counts towards the run being parked as `stuck`.

use std::sync::atomic::Ordering;

use chrono::{Duration, Utc};

use super::{run_input, step_error, step_result, WorkflowEngine, BLOCKED_RETRY_SECS};
use super::{JOURNAL_PAGE_BYTES, JOURNAL_PAGE_ENTRIES};
use crate::error::{WorkflowError, WorkflowResult};
use crate::executor::{Invocation, InvokeReply};
use crate::payload::PayloadField;
use crate::protocol::{InvokeRequest, JournalEntry, PROTOCOL_VERSION};
use crate::store::{ClaimedRun, RunRow, WorkflowLimit};
use crate::types::{RunStatus, StepKind, StepStatus, WorkflowSettings};

/// How soon a run deferred for its workflow's concurrency limit is due again.
/// By then the manifest is cached and the claim itself applies the limit.
const CONCURRENCY_RETRY_SECS: i64 = 2;

enum Failure {
    /// Nothing to invoke; not an attempt.
    Blocked(String),
    /// The workflow's concurrency limit is reached; not an attempt.
    Deferred(String),
    /// The invocation failed.
    Failed {
        category: &'static str,
        reason: String,
    },
}

impl From<WorkflowError> for Failure {
    fn from(error: WorkflowError) -> Self {
        Failure::Failed {
            category: "internal",
            reason: error.to_string(),
        }
    }
}

impl WorkflowEngine {
    /// Lease up to `batch` due runs.
    pub async fn claim(&self, batch: u64) -> WorkflowResult<Vec<ClaimedRun>> {
        let limits: Vec<WorkflowLimit> = self.manifests.limits();
        self.store.claim_due(batch, &limits).await
    }

    /// Invoke a claimed run and settle it. Never fails: every error ends as
    /// a fenced write on the run, or is left to the lease reaper.
    pub async fn invoke(&self, claim: ClaimedRun) {
        let run = claim.run;
        let lease = run.lease_token;
        match self.try_invoke(&run).await {
            Ok(()) => {}
            Err(Failure::Blocked(reason)) => {
                tracing::info!(run_id = %run.id, environment_id = run.environment_id, %reason, "workflow run blocked: nothing to invoke");
                let retry_at = Utc::now() + Duration::seconds(BLOCKED_RETRY_SECS);
                if let Err(error) = self
                    .store
                    .block_claimed(run.id, lease, claim.previous_status, &reason, retry_at)
                    .await
                {
                    tracing::error!(run_id = %run.id, %error, "could not give back a blocked workflow run; its lease expiry will");
                }
            }
            Err(Failure::Deferred(reason)) => {
                tracing::debug!(run_id = %run.id, environment_id = run.environment_id, %reason, "workflow run deferred");
                let retry_at = Utc::now() + Duration::seconds(CONCURRENCY_RETRY_SECS);
                if let Err(error) = self
                    .store
                    .block_claimed(run.id, lease, claim.previous_status, &reason, retry_at)
                    .await
                {
                    tracing::error!(run_id = %run.id, %error, "could not give back a deferred workflow run; its lease expiry will");
                }
            }
            Err(Failure::Failed { category, reason }) => {
                self.metrics.invocation_failed(
                    run.project_id,
                    run.environment_id,
                    &run.workflow_name,
                    category,
                );
                match self.store.fail_invocation(run.id, lease, &reason).await {
                    Ok(Some(RunStatus::Stuck)) => tracing::warn!(
                        run_id = %run.id,
                        project_id = run.project_id,
                        environment_id = run.environment_id,
                        workflow = %run.workflow_name,
                        %reason,
                        "workflow run is stuck after repeated invocation failures"
                    ),
                    Ok(Some(status)) => tracing::info!(
                        run_id = %run.id,
                        status = status.as_str(),
                        category,
                        %reason,
                        "workflow invocation failed; retrying with backoff"
                    ),
                    // The lease moved on (cancelled, reaped): nothing to do.
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(run_id = %run.id, %error, "could not record a failed workflow invocation")
                    }
                }
            }
        }
    }

    async fn try_invoke(&self, run: &RunRow) -> Result<(), Failure> {
        let settings = self.store.settings(run.environment_id).await?;
        let set = self
            .endpoints
            .endpoints(
                run.project_id,
                run.environment_id,
                settings.host_service.as_deref(),
            )
            .await
            .map_err(|error| Failure::Failed {
                category: "endpoints",
                reason: error.to_string(),
            })?;
        if set.endpoints.is_empty() {
            return Err(Failure::Blocked(
                set.unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "No healthy replica of the service is running".to_string()),
            ));
        }
        let manifest = self
            .manifests
            .get(
                &self.client,
                &self.signer,
                run.project_id,
                run.environment_id,
                &set,
            )
            .await
            .map_err(|reason| Failure::Failed {
                category: "manifest",
                reason,
            })?;
        let Some(served) = manifest
            .workflows
            .iter()
            .find(|workflow| workflow.name == run.workflow_name)
        else {
            let served: Vec<&str> = manifest
                .workflows
                .iter()
                .map(|workflow| workflow.name.as_str())
                .take(20)
                .collect();
            return Err(Failure::Failed {
                category: "not_served",
                reason: format!(
                    "The deployed service does not define workflow '{}' (it defines: {})",
                    run.workflow_name,
                    if served.is_empty() {
                        "none".to_string()
                    } else {
                        served.join(", ")
                    }
                ),
            });
        };
        // A claim made before this environment's manifest was cached could
        // not apply the workflow's limit: check it now, before any code runs.
        if let Some(limit) = served.concurrency {
            let running = self
                .store
                .running_count(run.environment_id, &run.workflow_name, run.id)
                .await?;
            if running >= i64::from(limit) {
                return Err(Failure::Deferred(format!(
                    "Waiting for a slot: workflow '{}' runs at most {limit} at once and {running} are running",
                    run.workflow_name
                )));
            }
        }

        let now = Utc::now();
        let deadline_at = now + Duration::seconds(i64::from(settings.invocation_deadline_secs));
        let input = self
            .codec
            .open(
                &Self::scope(run),
                &PayloadField::RunInput,
                &run_input(run),
                &settings,
            )
            .await?;
        let (journal, journal_next_sequence) = self.journal_entries(run, 0, &settings).await?;
        let request = InvokeRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: run.id,
            workflow: run.workflow_name.clone(),
            attempt: run.attempt,
            lease: run.lease_token,
            now,
            deadline_at,
            lease_expires_at: run
                .lease_expires_at
                .unwrap_or(now + Duration::seconds(i64::from(settings.lease_secs))),
            input,
            journal,
            journal_next_sequence,
        };
        let body = serde_json::to_vec(&request).map_err(|error| Failure::Failed {
            category: "internal",
            reason: format!("the invoke request did not serialize: {error}"),
        })?;
        let index = self.next_endpoint.fetch_add(1, Ordering::Relaxed) % set.endpoints.len();
        let endpoint = &set.endpoints[index];
        let max_response_bytes = usize::try_from(settings.max_payload_bytes)
            .unwrap_or(0)
            .saturating_mul(2)
            .saturating_add(64 * 1024);
        tracing::debug!(run_id = %run.id, attempt = run.attempt, lease = run.lease_token, endpoint = %endpoint.base_url, "invoking workflow run");
        let reply = self
            .client
            .invoke(
                &self.signer,
                Invocation {
                    project_id: run.project_id,
                    environment_id: run.environment_id,
                    run_id: run.id,
                    lease: run.lease_token,
                    deadline_at,
                    body,
                    max_response_bytes,
                    endpoint,
                },
            )
            .await
            .map_err(|error| {
                // A deployment change may have moved the endpoint.
                if matches!(error.category(), "connect" | "http_status") {
                    self.manifests.invalidate(run.environment_id);
                }
                Failure::Failed {
                    category: error.category(),
                    reason: error.to_string(),
                }
            })?;
        match reply {
            InvokeReply::Accepted => Ok(()),
            InvokeReply::Outcome(outcome) => {
                match self
                    .apply_outcome(run, run.lease_token, outcome.outcome)
                    .await
                {
                    Ok(_) => Ok(()),
                    // The run moved on while the invocation ran (cancelled,
                    // reaped, already reported through the callback).
                    Err(WorkflowError::StaleLease { .. }) => Ok(()),
                    Err(error @ WorkflowError::Validation { .. }) => Err(Failure::Failed {
                        category: "protocol",
                        reason: format!("the executor reported an invalid outcome: {error}"),
                    }),
                    Err(error) => {
                        // Infrastructure (database, blob storage): the lease
                        // expiry retries the run.
                        tracing::error!(run_id = %run.id, %error, "could not apply a workflow outcome");
                        Ok(())
                    }
                }
            }
        }
    }

    /// Decode the journal from `from` on, within the page bounds; returns the
    /// entries and, when more remain, the next sequence to fetch.
    pub(crate) async fn journal_entries(
        &self,
        run: &RunRow,
        from: i32,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<(Vec<JournalEntry>, Option<i32>)> {
        let rows = self
            .store
            .journal(run.id, from, JOURNAL_PAGE_ENTRIES)
            .await?;
        let full_page = rows.len() as u64 == JOURNAL_PAGE_ENTRIES;
        let scope = Self::scope(run);
        let mut selected = Vec::with_capacity(rows.len());
        let mut bytes = 0usize;
        let mut next_sequence = None;
        for row in rows {
            let size = usize::try_from(i64::from(row.result_bytes) + i64::from(row.error_bytes))
                .unwrap_or(0);
            if !selected.is_empty() && bytes + size > JOURNAL_PAGE_BYTES {
                next_sequence = Some(row.sequence);
                break;
            }
            bytes += size;
            let next = row.sequence + 1;
            selected.push(row);
            if full_page && selected.len() as u64 == JOURNAL_PAGE_ENTRIES {
                next_sequence = Some(next);
            }
        }
        let rows = self.store.hydrate_journal(run.id, selected).await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            entries.push(self.journal_entry(&scope, &row, settings).await?);
        }
        Ok((entries, next_sequence))
    }

    pub(crate) async fn journal_entry(
        &self,
        scope: &crate::payload::PayloadScope,
        row: &crate::store::StepRow,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<JournalEntry> {
        let result = self
            .codec
            .open(
                scope,
                &PayloadField::StepResult(row.sequence),
                &step_result(row),
                settings,
            )
            .await?;
        let error = self
            .codec
            .open(
                scope,
                &PayloadField::StepError(row.sequence),
                &step_error(row),
                settings,
            )
            .await?
            .and_then(|value| value.as_str().map(str::to_string));
        Ok(JournalEntry {
            sequence: row.sequence,
            name: row.name.clone(),
            kind: StepKind::parse(&row.kind).unwrap_or(StepKind::Step),
            status: StepStatus::parse(&row.status).unwrap_or(StepStatus::Pending),
            attempts: row.attempts,
            result,
            error,
            wake_at: row.wake_at,
        })
    }
}
