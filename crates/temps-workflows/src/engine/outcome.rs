// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applying how an invocation ended (ADR-106 §4): every outcome releases the
//! lease with a write fenced by it, so an outcome reported by an invocation
//! that already lost its lease changes nothing.

use chrono::Utc;
use serde_json::Value;

use uuid::Uuid;

use super::events::inline_only;
use super::WorkflowEngine;
use crate::error::{WorkflowError, WorkflowResult};
use crate::payload::{PayloadField, StoredPayload};
use crate::protocol::{Outcome, CHILD_EVENT, RESERVED_EVENT_PREFIX};
use crate::store::{Release, RunRow, WaitingRun};
use crate::types::{
    validate_key, validate_name, StepKind, StepStatus, WorkflowSettings, MAX_EVENT_NAME,
};

/// Cut `message` so its JSON encoding fits `limit` bytes, on a character
/// boundary.
pub(crate) fn fit_message(message: &str, limit: usize) -> String {
    let mut end = message.len().min(limit.min(super::MAX_ERROR_MESSAGE_BYTES));
    loop {
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = &message[..end];
        // A JSON string adds two quotes and may escape characters.
        let encoded = serde_json::to_string(candidate)
            .map(|s| s.len())
            .unwrap_or(usize::MAX);
        if encoded <= limit || end == 0 {
            return candidate.to_string();
        }
        end /= 2;
    }
}

impl WorkflowEngine {
    /// Seal an error message for a column without a blob key.
    pub(crate) async fn seal_message(
        &self,
        run: &RunRow,
        field: &PayloadField,
        message: &str,
        lease: i64,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<StoredPayload> {
        let settings = inline_only(settings);
        let limit = usize::try_from(settings.inline_payload_limit_bytes).unwrap_or(1024);
        let value = Value::String(fit_message(message, limit));
        self.codec
            .seal(&Self::scope(run), field, &value, lease, &settings)
            .await
    }

    /// The run failed for a reason Temps decided (oversize output, an
    /// incompatible replay): record `message` as its error.
    async fn fail_run(
        &self,
        run: &RunRow,
        lease: i64,
        kind: &'static str,
        message: &str,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<RunRow> {
        let error = self
            .seal_message(run, &PayloadField::RunError, message, lease, settings)
            .await?;
        let released = self
            .store
            .release(run.id, lease, Release::Failed { error, kind })
            .await?;
        self.metrics.run_finished(
            run.project_id,
            run.environment_id,
            &run.workflow_name,
            "failed",
        );
        tracing::info!(
            run_id = %run.id,
            project_id = run.project_id,
            environment_id = run.environment_id,
            workflow = %run.workflow_name,
            error_kind = kind,
            "workflow run failed"
        );
        self.child_finished(&released).await;
        Ok(released)
    }

    /// A suspension on a child must name a child this run started for the
    /// pending `child` entry at `sequence`.
    async fn require_child(&self, run: &RunRow, sequence: i32, key: &str) -> WorkflowResult<()> {
        self.require_pending(run, sequence, StepKind::Child).await?;
        let child = match key.parse::<Uuid>() {
            Ok(child_id) => self.store.get_run(child_id).await?,
            Err(_) => None,
        };
        let entry = self.store.step(run.id, sequence).await?;
        let recorded = match entry {
            Some(entry) => {
                self.journal_entry(
                    &Self::scope(run),
                    &entry,
                    &self.store.settings(run.environment_id).await?,
                )
                .await?
                .result
            }
            None => None,
        };
        let started_here = child.as_ref().is_some_and(|child| {
            child.parent_run_id == Some(run.id)
                && recorded
                    .as_ref()
                    .and_then(|v| v.get("run_id"))
                    .and_then(Value::as_str)
                    == Some(key)
        });
        if started_here {
            Ok(())
        } else {
            Err(WorkflowError::Validation {
                field: "outcome.key",
                reason: format!(
                    "run {} suspended on child '{key}', which is not the run its entry {sequence} started",
                    run.id
                ),
            })
        }
    }

    /// The pending journal entry a suspension refers to must exist.
    async fn require_pending(
        &self,
        run: &RunRow,
        sequence: i32,
        kind: StepKind,
    ) -> WorkflowResult<()> {
        let entry = self.store.step(run.id, sequence).await?;
        let matches = entry.as_ref().is_some_and(|entry| {
            entry.kind == kind.as_str() && entry.status == StepStatus::Pending.as_str()
        });
        if matches {
            Ok(())
        } else {
            Err(WorkflowError::Validation {
                field: "outcome.sequence",
                reason: format!(
                    "run {} suspended on entry {sequence}, which is not a pending {} entry; record it first",
                    run.id,
                    kind.as_str()
                ),
            })
        }
    }

    /// Apply `outcome` to `run`, held under `lease`.
    pub async fn apply_outcome(
        &self,
        run: &RunRow,
        lease: i64,
        outcome: Outcome,
    ) -> WorkflowResult<RunRow> {
        let settings = self.store.settings(run.environment_id).await?;
        match outcome {
            Outcome::Completed { output } => {
                let value = output.unwrap_or(Value::Null);
                let sealed = match self
                    .codec
                    .seal(
                        &Self::scope(run),
                        &PayloadField::RunOutput,
                        &value,
                        lease,
                        &settings,
                    )
                    .await
                {
                    Ok(sealed) => sealed,
                    Err(error @ WorkflowError::PayloadTooLarge { .. }) => {
                        return self
                            .fail_run(
                                run,
                                lease,
                                "payload_too_large",
                                &error.to_string(),
                                &settings,
                            )
                            .await;
                    }
                    Err(error) => return Err(error),
                };
                match self
                    .store
                    .release(
                        run.id,
                        lease,
                        Release::Completed {
                            output: sealed.clone(),
                        },
                    )
                    .await
                {
                    Ok(released) => {
                        self.metrics.run_finished(
                            run.project_id,
                            run.environment_id,
                            &run.workflow_name,
                            "completed",
                        );
                        tracing::info!(
                            run_id = %run.id,
                            project_id = run.project_id,
                            environment_id = run.environment_id,
                            workflow = %run.workflow_name,
                            "workflow run completed"
                        );
                        self.child_finished(&released).await;
                        Ok(released)
                    }
                    Err(error) => {
                        self.codec.discard(run.project_id, &sealed).await;
                        Err(error)
                    }
                }
            }
            Outcome::Failed { error, sequence } => {
                let kind = if sequence.is_some() {
                    "step_failed"
                } else {
                    "workflow_failed"
                };
                self.fail_run(run, lease, kind, &error, &settings).await
            }
            Outcome::Sleep { sequence, until } => {
                self.require_pending(run, sequence, StepKind::Sleep).await?;
                let released = self
                    .store
                    .release(run.id, lease, Release::Sleep { until })
                    .await?;
                tracing::debug!(run_id = %run.id, %until, "workflow run sleeping");
                Ok(released)
            }
            Outcome::Wait {
                sequence,
                event,
                key,
                timeout_at,
            } => {
                validate_name("outcome.event", &event, MAX_EVENT_NAME)?;
                let key = key.unwrap_or_default();
                if !key.is_empty() {
                    validate_key("outcome.key", &key)?;
                }
                let on_child = event == CHILD_EVENT;
                if on_child {
                    if timeout_at.is_some() {
                        return Err(WorkflowError::Validation {
                            field: "outcome.timeout_at",
                            reason: format!(
                                "run {} suspended on child '{key}' with a timeout; a child wait lasts until the child finishes",
                                run.id
                            ),
                        });
                    }
                    self.require_child(run, sequence, &key).await?;
                } else if event.starts_with(RESERVED_EVENT_PREFIX) {
                    return Err(WorkflowError::Validation {
                        field: "outcome.event",
                        reason: format!(
                            "run {} waits on '{event}', which starts with '{RESERVED_EVENT_PREFIX}', reserved for events Temps sends itself",
                            run.id
                        ),
                    });
                } else {
                    self.require_pending(run, sequence, StepKind::Wait).await?;
                }
                let released = self
                    .store
                    .release(
                        run.id,
                        lease,
                        Release::Wait {
                            sequence,
                            event: event.clone(),
                            key: key.clone(),
                            timeout_at,
                        },
                    )
                    .await?;
                let waiting = WaitingRun {
                    run_id: run.id,
                    project_id: run.project_id,
                    environment_id: run.environment_id,
                    lease,
                    sequence,
                    event_name: event,
                    event_key: key,
                };
                if on_child {
                    // A child that finished before the run suspended.
                    if let Err(error) = self.deliver_finished_child(&waiting).await {
                        tracing::warn!(run_id = %run.id, %error, "could not deliver a finished child's result; reconciliation retries it");
                    }
                } else if let Err(error) = self.deliver_buffered(&waiting).await {
                    // An event that arrived before the run suspended.
                    tracing::warn!(run_id = %run.id, %error, "could not deliver a buffered workflow event; reconciliation retries it");
                }
                Ok(released)
            }
            Outcome::Retry { sequence: _, at } => {
                let at = at.max(Utc::now());
                self.store
                    .release(run.id, lease, Release::Retry { at })
                    .await
            }
            Outcome::Incompatible {
                sequence,
                expected,
                found,
            } => {
                let message = WorkflowError::Incompatible {
                    run_id: run.id,
                    sequence,
                    expected,
                    found,
                }
                .to_string();
                tracing::warn!(run_id = %run.id, %message, "workflow run is incompatible with the deployed code");
                self.fail_run(run, lease, "incompatible", &message, &settings)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fit_message;

    #[test]
    fn messages_are_cut_to_fit_on_character_boundaries() {
        assert_eq!(fit_message("short", 100), "short");
        let long = "é".repeat(1000);
        let cut = fit_message(&long, 101);
        assert!(serde_json::to_string(&cut).unwrap().len() <= 101);
        assert!(cut.chars().all(|c| c == 'é'));
        // Escapes count against the limit.
        let quotes = "\"".repeat(100);
        assert!(
            serde_json::to_string(&fit_message(&quotes, 50))
                .unwrap()
                .len()
                <= 50
        );
    }
}
