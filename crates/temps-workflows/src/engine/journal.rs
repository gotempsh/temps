// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The executor's callbacks (ADR-103 §3, §11): read the journal, record an
//! entry, heartbeat, report an outcome. Each is authorized by the invocation
//! token Temps signed for this run and lease, and fenced by that lease.

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use super::events::EventSpec;
use super::outcome::fit_message;
use super::WorkflowEngine;
use crate::error::{WorkflowError, WorkflowResult};
use crate::payload::{PayloadField, StoredPayload};
use crate::protocol::{
    HeartbeatResponse, JournalPage, OutcomeRequest, OutcomeResponse, StepWriteRequest,
    StepWriteResponse,
};
use crate::store::{RunRow, StepLimits, StepRecord, StepRecordOutcome};
use crate::token::InvocationClaims;
use crate::types::{validate_name, RunStatus, StepKind, StepStatus};

/// Longest step name.
pub const MAX_STEP_NAME: usize = 200;

/// A verified invocation token and the run it authorizes.
pub struct Authorized {
    pub claims: InvocationClaims,
    pub lease: i64,
    pub run: RunRow,
}

impl WorkflowEngine {
    /// Verify an invocation token for `run_id`.
    pub async fn authorize(&self, token: &str, run_id: Uuid) -> WorkflowResult<Authorized> {
        let (claims, lease) = self.signer.verify_run(token, run_id, Utc::now())?;
        let run = self
            .store
            .get_run(run_id)
            .await?
            .ok_or(WorkflowError::RunNotFound {
                project_id: claims.pid,
                environment_id: claims.eid,
                run_id,
            })?;
        if run.project_id != claims.pid || run.environment_id != claims.eid {
            return Err(WorkflowError::InvalidToken {
                reason: "token is for another environment",
            });
        }
        Ok(Authorized { claims, lease, run })
    }

    fn require_lease(auth: &Authorized) -> WorkflowResult<()> {
        if auth.run.lease_token == auth.lease && auth.run.status == RunStatus::Running.as_str() {
            Ok(())
        } else {
            Err(WorkflowError::StaleLease {
                run_id: auth.run.id,
                presented: auth.lease,
                current: auth.run.lease_token,
                status: auth.run.status.clone(),
            })
        }
    }

    /// `GET /workflow-runs/{run_id}/journal?from=N`.
    pub async fn journal_page(
        &self,
        token: &str,
        run_id: Uuid,
        from: i32,
    ) -> WorkflowResult<JournalPage> {
        let auth = self.authorize(token, run_id).await?;
        Self::require_lease(&auth)?;
        if from < 0 {
            return Err(WorkflowError::Validation {
                field: "from",
                reason: "must be zero or more".into(),
            });
        }
        let settings = self.store.settings(auth.run.environment_id).await?;
        let (entries, next_sequence) = self.journal_entries(&auth.run, from, &settings).await?;
        Ok(JournalPage {
            entries,
            next_sequence,
        })
    }

    fn validate_write(run_id: Uuid, write: &StepWriteRequest) -> WorkflowResult<()> {
        if write.sequence < 0 {
            return Err(WorkflowError::Validation {
                field: "sequence",
                reason: "must be zero or more".into(),
            });
        }
        validate_name("step name", &write.name, MAX_STEP_NAME)?;
        if write.attempts < 1 {
            return Err(WorkflowError::Validation {
                field: "attempts",
                reason: "must be at least 1".into(),
            });
        }
        let invalid = |reason: String| WorkflowError::Validation {
            field: "kind",
            reason,
        };
        if write.kind == StepKind::Sleep
            && write.status == StepStatus::Pending
            && write.wake_at.is_none()
        {
            return Err(invalid(format!(
                "pending sleep {} of run {run_id} needs wake_at",
                write.sequence
            )));
        }
        if matches!(
            write.kind,
            StepKind::Now | StepKind::Random | StepKind::SendEvent
        ) && write.status == StepStatus::Pending
        {
            return Err(invalid(format!(
                "a {} entry is recorded completed, never pending",
                write.kind.as_str()
            )));
        }
        match (write.kind, &write.event) {
            (StepKind::SendEvent, None) if write.status == StepStatus::Completed => {
                return Err(invalid("a send_event entry carries its event".into()))
            }
            (kind, Some(_)) if kind != StepKind::SendEvent => {
                return Err(invalid(format!(
                    "only a send_event entry carries an event, not {}",
                    kind.as_str()
                )))
            }
            _ => {}
        }
        if write.trace_id.as_ref().is_some_and(|id| id.len() > 64) {
            return Err(WorkflowError::Validation {
                field: "trace_id",
                reason: "longer than 64 characters".into(),
            });
        }
        if write.error_ref.as_ref().is_some_and(|id| id.len() > 128) {
            return Err(WorkflowError::Validation {
                field: "error_ref",
                reason: "longer than 128 characters".into(),
            });
        }
        Ok(())
    }

    /// `POST /workflow-runs/{run_id}/steps`.
    pub async fn record_step(
        self: &Arc<Self>,
        token: &str,
        run_id: Uuid,
        write: StepWriteRequest,
    ) -> WorkflowResult<StepWriteResponse> {
        let auth = self.authorize(token, run_id).await?;
        Self::require_lease(&auth)?;
        Self::validate_write(run_id, &write)?;
        let run = &auth.run;
        let settings = self.store.settings(run.environment_id).await?;
        let scope = Self::scope(run);

        let result = match &write.result {
            Some(value) => {
                self.codec
                    .seal(
                        &scope,
                        &PayloadField::StepResult(write.sequence),
                        value,
                        auth.lease,
                        &settings,
                    )
                    .await?
            }
            None => StoredPayload::default(),
        };
        let error = match &write.error {
            Some(message) => {
                let limit = usize::try_from(settings.inline_payload_limit_bytes).unwrap_or(1024);
                self.seal_message(
                    run,
                    &PayloadField::StepError(write.sequence),
                    &fit_message(message, limit),
                    auth.lease,
                    &settings,
                )
                .await?
            }
            None => StoredPayload::default(),
        };
        let event = match &write.event {
            Some(event) => Some(
                self.seal_event(EventSpec {
                    project_id: run.project_id,
                    environment_id: run.environment_id,
                    name: &event.name,
                    key: event.key.as_deref().unwrap_or_default(),
                    payload: event.payload.as_ref(),
                    source: "workflow",
                    source_run_id: Some(run.id),
                })
                .await?,
            ),
            None => None,
        };
        let record = StepRecord {
            run_id,
            lease: auth.lease,
            sequence: write.sequence,
            name: write.name.clone(),
            kind: write.kind,
            status: write.status,
            attempts: write.attempts,
            result: result.clone(),
            error,
            wake_at: write.wake_at,
            duration_ms: write.duration_ms,
            trace_id: write.trace_id.clone(),
            error_ref: write.error_ref.clone(),
            event,
        };
        let limits = StepLimits {
            max_steps: settings.max_steps_per_run,
            max_journal_bytes: settings.max_journal_bytes_per_run,
            lease_secs: settings.lease_secs,
        };
        let outcome = match self.store.record_step(record, limits).await {
            Ok(outcome) => outcome,
            Err(error) => {
                self.codec.discard(run.project_id, &result).await;
                return Err(error);
            }
        };
        let (row, lease_expires_at, already_recorded, event_id) = match outcome {
            StepRecordOutcome::Recorded {
                entry,
                lease_expires_at,
                event_id,
            } => (entry, lease_expires_at, false, event_id),
            StepRecordOutcome::AlreadyRecorded {
                entry,
                lease_expires_at,
            } => {
                self.codec.discard(run.project_id, &result).await;
                (entry, lease_expires_at, true, None)
            }
        };
        if !already_recorded && write.status.is_final() {
            self.metrics.step_finished(
                run.project_id,
                run.environment_id,
                &run.workflow_name,
                write.status.as_str(),
                write.duration_ms,
            );
        }
        if let Some(event_id) = event_id {
            if let Some(event) = self.store.event(event_id).await? {
                self.deliver_in_background(event);
            }
        }
        let entry = self.journal_entry(&scope, &row, &settings).await?;
        Ok(StepWriteResponse {
            entry,
            already_recorded,
            lease_expires_at,
        })
    }

    /// `POST /workflow-runs/{run_id}/heartbeat`. A cancelled run answers
    /// `cancelled: true` so the executor stops at its next step.
    pub async fn heartbeat(&self, token: &str, run_id: Uuid) -> WorkflowResult<HeartbeatResponse> {
        let auth = self.authorize(token, run_id).await?;
        if auth.run.status == RunStatus::Cancelled.as_str() {
            return Ok(HeartbeatResponse {
                lease_expires_at: Utc::now(),
                cancelled: true,
            });
        }
        let lease_expires_at = self.store.heartbeat(run_id, auth.lease).await?;
        Ok(HeartbeatResponse {
            lease_expires_at,
            cancelled: false,
        })
    }

    /// `POST /workflow-runs/{run_id}/outcome`: the callback form of an
    /// invoke reply, for executors that answered `202`.
    pub async fn report_outcome(
        &self,
        token: &str,
        run_id: Uuid,
        request: OutcomeRequest,
    ) -> WorkflowResult<OutcomeResponse> {
        let auth = self.authorize(token, run_id).await?;
        Self::require_lease(&auth)?;
        let run = self
            .apply_outcome(&auth.run, auth.lease, request.outcome)
            .await?;
        Ok(OutcomeResponse {
            status: RunStatus::parse(&run.status).unwrap_or(RunStatus::Running),
        })
    }
}
