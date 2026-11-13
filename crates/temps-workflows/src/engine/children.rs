// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Child workflows (ADR-106 §4, M2).
//!
//! A `child` journal entry starts a run of another workflow in the same
//! environment. The child is started exactly once: its idempotency key is
//! the parent's run id and the entry's sequence, so a retried write finds
//! the child it already started. The parent records the entry `pending`
//! with `{"run_id": ...}` and suspends on it ([`CHILD_EVENT`], keyed by the
//! child's id). When the child finishes, Temps completes the entry with
//! [`child_result`] through the same fenced, quota-checked completion as an
//! event delivery, and the parent is due again. A child that finished before
//! its parent suspended is delivered when the parent suspends, and a crash
//! in between is repaired by [`WorkflowEngine::reconcile_children`].

use serde_json::Value;
use uuid::Uuid;

use super::{run_error, run_output, WorkflowEngine};
use crate::error::{WorkflowError, WorkflowResult};
use crate::payload::{PayloadField, PayloadScope};
use crate::protocol::{child_result, ChildStart, CHILD_EVENT};
use crate::service::MAX_WORKFLOW_NAME;
use crate::store::{NewRun, RunRow, StartOutcome, Starter, WaitingRun};
use crate::types::{validate_name, RunStatus, WorkflowSettings};

/// The idempotency key of the child a parent's entry starts.
pub(crate) fn child_key(parent_run_id: Uuid, sequence: i32) -> String {
    format!("child:{parent_run_id}:{sequence}")
}

impl WorkflowEngine {
    /// Start the child `child` names for entry `sequence` of `parent`, or
    /// find the one an earlier write of the entry started. Returns its id.
    pub(crate) async fn start_child(
        &self,
        parent: &RunRow,
        sequence: i32,
        child: &ChildStart,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<Uuid> {
        validate_name("child.workflow", &child.workflow, MAX_WORKFLOW_NAME)?;
        let id = Uuid::new_v4();
        let scope = PayloadScope {
            project_id: parent.project_id,
            environment_id: parent.environment_id,
            run_id: id,
        };
        let input = match &child.input {
            Some(input) => {
                self.codec
                    .seal(&scope, &PayloadField::RunInput, input, 0, settings)
                    .await?
            }
            None => Default::default(),
        };
        let outcome = self
            .store
            .insert_run(
                NewRun {
                    id,
                    project_id: parent.project_id,
                    environment_id: parent.environment_id,
                    workflow_name: child.workflow.clone(),
                    input: input.clone(),
                    idempotency_key: Some(child_key(parent.id, sequence)),
                    deployment_id: None,
                    starter: Starter::Machine,
                    parent_run_id: Some(parent.id),
                },
                i64::from(settings.max_active_runs),
            )
            .await;
        match outcome {
            Ok(StartOutcome::Created(run)) => {
                self.metrics
                    .run_started(run.project_id, run.environment_id, &run.workflow_name);
                tracing::info!(
                    run_id = %run.id,
                    parent_run_id = %parent.id,
                    project_id = run.project_id,
                    environment_id = run.environment_id,
                    workflow = %run.workflow_name,
                    "child workflow run started"
                );
                Ok(run.id)
            }
            Ok(StartOutcome::Existing(run)) => {
                self.codec.discard(parent.project_id, &input).await;
                if run.parent_run_id != Some(parent.id) {
                    return Err(WorkflowError::Validation {
                        field: "child",
                        reason: format!(
                            "entry {sequence} of run {} would start a child of workflow '{}', \
                             but run {} already holds its idempotency key and is not its child",
                            parent.id, child.workflow, run.id
                        ),
                    });
                }
                Ok(run.id)
            }
            Err(error) => {
                self.codec.discard(parent.project_id, &input).await;
                Err(error)
            }
        }
    }

    /// `child` just finished (completed, failed or cancelled): wake its
    /// parent if it is suspended on it. Failures are logged; the
    /// reconciliation pass retries them.
    pub(crate) async fn child_finished(&self, child: &RunRow) {
        let Some(parent_id) = child.parent_run_id else {
            return;
        };
        let parent = match self.store.get_run(parent_id).await {
            Ok(Some(parent)) => parent,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(run_id = %child.id, %parent_id, %error, "could not load the parent of a finished child run; reconciliation retries it");
                return;
            }
        };
        let Some(waiting) = waiting_on_child(&parent, child.id) else {
            return;
        };
        if let Err(error) = self.deliver_child(&waiting, Some(child)).await {
            tracing::warn!(run_id = %child.id, %parent_id, %error, "could not wake the parent of a finished child run; reconciliation retries it");
        }
    }

    /// The run behind `waiting` just suspended on a child: deliver the
    /// child's result now if it already finished.
    pub(crate) async fn deliver_finished_child(
        &self,
        waiting: &WaitingRun,
    ) -> WorkflowResult<bool> {
        let Ok(child_id) = waiting.event_key.parse::<Uuid>() else {
            return Err(WorkflowError::Validation {
                field: "outcome.key",
                reason: format!(
                    "run {} suspended on child '{}', which is not a run id",
                    waiting.run_id, waiting.event_key
                ),
            });
        };
        let child = self.store.get_run(child_id).await?;
        match &child {
            Some(child) if !is_finished(child) => Ok(false),
            _ => self.deliver_child(waiting, child.as_ref()).await,
        }
    }

    /// Complete the parent's `child` entry with the child's result. `None`:
    /// the child was deleted, which the parent sees as status `deleted`.
    async fn deliver_child(
        &self,
        waiting: &WaitingRun,
        child: Option<&RunRow>,
    ) -> WorkflowResult<bool> {
        let child_id = waiting.event_key.parse::<Uuid>().unwrap_or_default();
        let value = match child {
            Some(child) => self.finished_child_result(child).await?,
            None => child_result(
                child_id,
                "deleted",
                None,
                Some(format!(
                    "child run {child_id} was deleted before it finished"
                )),
            ),
        };
        self.complete_waiting(
            waiting,
            None,
            &value,
            &format!("the result of child run {child_id}"),
        )
        .await
    }

    async fn finished_child_result(&self, child: &RunRow) -> WorkflowResult<Value> {
        let settings = self.store.settings(child.environment_id).await?;
        let scope = Self::scope(child);
        let output = if child.status == RunStatus::Completed.as_str() {
            self.codec
                .open(
                    &scope,
                    &PayloadField::RunOutput,
                    &run_output(child),
                    &settings,
                )
                .await?
        } else {
            None
        };
        let error = match self
            .codec
            .open(
                &scope,
                &PayloadField::RunError,
                &run_error(child),
                &settings,
            )
            .await?
        {
            Some(Value::String(message)) => Some(message),
            Some(other) => Some(other.to_string()),
            None if child.status == RunStatus::Cancelled.as_str() => {
                Some(format!("child run {} was cancelled", child.id))
            }
            None => None,
        };
        Ok(child_result(child.id, &child.status, output, error))
    }

    /// Wake parents suspended on children that already finished, or were
    /// deleted, when the wake-up a crash interrupted never came. Returns how
    /// many parents were woken.
    pub async fn reconcile_children(&self, limit: u64) -> WorkflowResult<u64> {
        let mut woken = 0;
        for waiting in self.store.waits_on_finished_children(limit).await? {
            match self.deliver_finished_child(&waiting).await {
                Ok(true) => woken += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(run_id = %waiting.run_id, %error, "could not reconcile a child workflow wait")
                }
            }
        }
        if woken > 0 {
            tracing::info!(woken, "reconciled interrupted child workflow deliveries");
        }
        Ok(woken)
    }
}

fn is_finished(run: &RunRow) -> bool {
    RunStatus::parse(&run.status).is_some_and(|status| {
        matches!(
            status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
    })
}

/// `parent`, if it is suspended on `child_id`.
fn waiting_on_child(parent: &RunRow, child_id: Uuid) -> Option<WaitingRun> {
    let key = child_id.to_string();
    if parent.status != RunStatus::Waiting.as_str()
        || parent.awaited_event_name.as_deref() != Some(CHILD_EVENT)
        || parent.awaited_event_key.as_deref() != Some(key.as_str())
    {
        return None;
    }
    Some(WaitingRun {
        run_id: parent.id,
        project_id: parent.project_id,
        environment_id: parent.environment_id,
        lease: parent.lease_token,
        sequence: parent.awaited_sequence?,
        event_name: CHILD_EVENT.to_string(),
        event_key: key,
    })
}
