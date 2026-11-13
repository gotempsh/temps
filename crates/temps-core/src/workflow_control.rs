// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the rest of Temps may ask of the durable workflows engine
//! (ADR-103 §9, M2/M3).
//!
//! `temps-workflows` implements [`WorkflowControl`]; the deployments crate
//! reads it to start a run on a cron schedule, to follow the runs a cron
//! started, and to warn about a deploy that would strand active runs. Neither
//! crate depends on the other, as with [`crate::ExecutorEndpoints`] in the
//! other direction.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// Start a run of `workflow` in an environment.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowStart {
    pub project_id: i32,
    pub environment_id: i32,
    pub workflow: String,
    pub input: Option<Value>,
    /// Starts with the same key return the run the first one created, so a
    /// retried start (a scheduler that crashed after starting) starts nothing
    /// new.
    pub idempotency_key: String,
}

/// A run [`WorkflowControl::start`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedWorkflowRun {
    pub run_id: Uuid,
    /// `false`: the idempotency key already had this run.
    pub created: bool,
}

/// Where a run is, coarsely: enough for a caller that started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRunPhase {
    /// Queued, running, sleeping, waiting, retrying or stuck.
    Active,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowRunPhase {
    pub fn is_finished(self) -> bool {
        !matches!(self, WorkflowRunPhase::Active)
    }
}

/// The phase of one run, with why it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunState {
    pub run_id: Uuid,
    pub workflow: String,
    pub phase: WorkflowRunPhase,
    /// The run's status as Temps reports it (`waiting`, `stuck`, ...).
    pub status: String,
    /// The failure category (`step_failed`, `incompatible`, ...).
    pub error_kind: Option<String>,
}

/// Active runs of one workflow in an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorkflowRuns {
    pub workflow: String,
    pub active: u64,
}

#[derive(Debug, Error)]
pub enum WorkflowControlError {
    /// The start was refused for a reason the caller can show as is: an
    /// invalid workflow name, the environment's active-run quota.
    #[error("Starting workflow '{workflow}' in environment {environment_id} of project {project_id} was refused: {reason}")]
    Refused {
        project_id: i32,
        environment_id: i32,
        workflow: String,
        reason: String,
    },
    #[error("Workflow operation '{operation}' in environment {environment_id} of project {project_id} failed: {reason}")]
    Failed {
        project_id: i32,
        environment_id: i32,
        operation: &'static str,
        reason: String,
    },
}

#[async_trait]
pub trait WorkflowControl: Send + Sync {
    /// If enabled, pause new invocations while a deployment drains running attempts.
    async fn begin_deploy_drain(
        &self,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
    ) -> Result<Option<WorkflowDrain>, WorkflowControlError>;
    /// Renew the pause and report how many invocations still run.
    async fn renew_deploy_drain(
        &self,
        project_id: i32,
        environment_id: i32,
        token: Uuid,
    ) -> Result<u64, WorkflowControlError>;
    async fn end_deploy_drain(
        &self,
        project_id: i32,
        environment_id: i32,
        token: Uuid,
    ) -> Result<(), WorkflowControlError>;

    /// Find an already started occurrence before applying a replacement policy.
    async fn find_started(
        &self,
        project_id: i32,
        environment_id: i32,
        workflow: &str,
        idempotency_key: &str,
    ) -> Result<Option<Uuid>, WorkflowControlError>;

    /// Start a run, idempotently by [`WorkflowStart::idempotency_key`].
    async fn start(&self, start: WorkflowStart)
        -> Result<StartedWorkflowRun, WorkflowControlError>;

    /// The states of `run_ids` in the environment, in one query. A run that
    /// no longer exists (deleted, expired) is absent from the result.
    async fn run_states(
        &self,
        project_id: i32,
        environment_id: i32,
        run_ids: &[Uuid],
    ) -> Result<Vec<WorkflowRunState>, WorkflowControlError>;

    /// Cancel a run of the environment, and its children. `false`: it had
    /// already finished, or does not exist.
    async fn cancel(
        &self,
        project_id: i32,
        environment_id: i32,
        run_id: Uuid,
    ) -> Result<bool, WorkflowControlError>;

    /// Active runs per workflow in the environment.
    async fn active_runs(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<Vec<ActiveWorkflowRuns>, WorkflowControlError>;
}

/// A short lease prevents a crashed deployment process from pausing work forever.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowDrain {
    pub token: Uuid,
    pub timeout_secs: u32,
}
