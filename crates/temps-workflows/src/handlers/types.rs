// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request and response bodies of the workflows API. Responses never carry a
//! payload (input, output, step result, error message, event payload): only
//! its size and whether it exists. Payloads are read with the audited
//! reveal endpoint, or by the app through its workflow credential.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temps_core::{AuditLogger, ProjectAccessChecker};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::engine::WorkflowEngine;
use crate::protocol::InvocationKeyResponse;
use crate::service::{ExecutorStatus, Overview, WorkflowInfo, WorkflowService};
use crate::store::{CredentialRow, RunRow, StepRow};
use crate::types::{RunStatus, StepKind, StepStatus};

pub struct WorkflowsAppState {
    pub service: Arc<WorkflowService>,
    pub engine: Arc<WorkflowEngine>,
    pub invocation_key: InvocationKeyResponse,
    pub audit_service: Arc<dyn AuditLogger>,
    pub project_access_checker: Option<Arc<dyn ProjectAccessChecker>>,
}

// ── Overview ─────────────────────────────────────────────────────────

/// Whether the environment can run workflows. When `configured` is false,
/// `reason` says what is missing and `setup_path` where to fix it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowExecutorCapabilityResponse)]
pub struct ExecutorCapabilityResponse {
    pub configured: bool,
    pub reason: Option<String>,
    pub setup_path: Option<String>,
    /// The deployment whose replicas would be invoked.
    pub deployment_id: Option<i32>,
    /// Healthy replicas found.
    pub replicas: usize,
    /// SDK name and version reported by the manifest.
    pub sdk: Option<String>,
}

impl From<ExecutorStatus> for ExecutorCapabilityResponse {
    fn from(status: ExecutorStatus) -> Self {
        Self {
            configured: status.configured,
            reason: status.reason,
            setup_path: status.setup_path,
            deployment_id: status.deployment_id,
            replicas: status.replicas,
            sdk: status.sdk,
        }
    }
}

/// The environment's workflow credential, never its token.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowCredentialStatusResponse)]
pub struct CredentialStatusResponse {
    pub configured: bool,
    /// First characters of the token, to recognise it.
    pub prefix: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<Option<CredentialRow>> for CredentialStatusResponse {
    fn from(row: Option<CredentialRow>) -> Self {
        match row {
            Some(row) => Self {
                configured: true,
                prefix: Some(row.token_prefix),
                created_at: Some(row.created_at),
                last_used_at: row.last_used_at,
            },
            None => Self {
                configured: false,
                prefix: None,
                created_at: None,
                last_used_at: None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowSummaryResponse {
    pub name: String,
    pub version: Option<String>,
    /// Runs allowed to execute at once, from the manifest.
    pub concurrency: Option<u32>,
    /// False when runs exist for a workflow the deployment no longer defines.
    pub defined: bool,
    pub active_runs: i64,
    pub completed_runs: i64,
    pub failed_runs: i64,
    pub last_run_at: Option<DateTime<Utc>>,
}

impl From<WorkflowInfo> for WorkflowSummaryResponse {
    fn from(info: WorkflowInfo) -> Self {
        Self {
            name: info.name,
            version: info.version,
            concurrency: info.concurrency,
            defined: info.defined,
            active_runs: info.active_runs,
            completed_runs: info.completed_runs,
            failed_runs: info.failed_runs,
            last_run_at: info.last_run_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowsOverviewResponse {
    pub executor: ExecutorCapabilityResponse,
    pub credential: CredentialStatusResponse,
    pub workflows: Vec<WorkflowSummaryResponse>,
}

impl From<Overview> for WorkflowsOverviewResponse {
    fn from(overview: Overview) -> Self {
        Self {
            executor: overview.executor.into(),
            credential: overview.credential.into(),
            workflows: overview.workflows.into_iter().map(Into::into).collect(),
        }
    }
}

// ── Runs ─────────────────────────────────────────────────────────────

/// The event a waiting run is suspended on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowAwaitedEventResponse)]
pub struct AwaitedEventResponse {
    pub name: String,
    pub key: Option<String>,
}

/// A run, payloads masked.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowRunResponse {
    pub id: Uuid,
    pub project_id: i32,
    pub environment_id: i32,
    pub workflow: String,
    pub status: RunStatus,
    /// Invocations so far.
    pub attempt: i32,
    pub consecutive_failures: i32,
    /// Category of the failure: `step_failed`, `workflow_failed`,
    /// `incompatible`, `payload_too_large`, `cancelled`.
    pub error_kind: Option<String>,
    /// Why the run is not progressing (no replica, invocation failures).
    pub blocked_reason: Option<String>,
    pub idempotency_key: Option<String>,
    pub parent_run_id: Option<Uuid>,
    pub started_by_user_id: Option<i32>,
    pub started_by_credential_id: Option<i32>,
    pub has_input: bool,
    pub input_bytes: i32,
    pub has_output: bool,
    pub output_bytes: i32,
    pub has_error: bool,
    pub step_count: i32,
    pub journal_bytes: i64,
    pub awaited_event: Option<AwaitedEventResponse>,
    /// When the run is next runnable.
    pub due_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl From<RunRow> for WorkflowRunResponse {
    fn from(run: RunRow) -> Self {
        let awaited_event = run
            .awaited_event_name
            .clone()
            .map(|name| AwaitedEventResponse {
                name,
                key: run.awaited_event_key.clone().filter(|key| !key.is_empty()),
            });
        Self {
            id: run.id,
            project_id: run.project_id,
            environment_id: run.environment_id,
            workflow: run.workflow_name,
            status: RunStatus::parse(&run.status).unwrap_or(RunStatus::Failed),
            attempt: run.attempt,
            consecutive_failures: run.consecutive_failures,
            error_kind: run.error_kind,
            blocked_reason: run.blocked_reason,
            idempotency_key: run.idempotency_key,
            parent_run_id: run.parent_run_id,
            started_by_user_id: run.started_by_user_id,
            started_by_credential_id: run.started_by_credential_id,
            has_input: run.input_encrypted.is_some() || run.input_blob_key.is_some(),
            input_bytes: run.input_bytes,
            has_output: run.output_encrypted.is_some() || run.output_blob_key.is_some(),
            output_bytes: run.output_bytes,
            has_error: run.error_encrypted.is_some(),
            step_count: run.step_count,
            journal_bytes: run.journal_bytes,
            awaited_event,
            due_at: run.due_at,
            lease_expires_at: run.lease_expires_at,
            created_at: run.created_at,
            updated_at: run.updated_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowRunListResponse {
    pub runs: Vec<WorkflowRunResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListRunsQuery {
    pub status: Option<RunStatus>,
    /// Only runs of this workflow.
    pub workflow: Option<String>,
    /// 1-indexed page number. Defaults to 1.
    pub page: Option<u64>,
    /// Items per page. Defaults to 20, capped at 100.
    pub page_size: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct PageQuery {
    /// 1-indexed page number. Defaults to 1.
    pub page: Option<u64>,
    /// Items per page. Defaults to 20, capped at 100.
    pub page_size: Option<u64>,
}

/// A journal entry, payloads masked.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowStepResponse {
    pub sequence: i32,
    pub name: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub attempts: i32,
    pub has_result: bool,
    pub result_bytes: i32,
    pub has_error: bool,
    /// A sleep's wake time or a wait's timeout.
    pub wake_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    /// OTEL trace of the attempt, to open in traces.
    pub trace_id: Option<String>,
    /// Error-tracking event of the attempt's exception.
    pub error_ref: Option<String>,
    /// The event that completed a wait.
    pub event_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl From<StepRow> for WorkflowStepResponse {
    fn from(step: StepRow) -> Self {
        Self {
            sequence: step.sequence,
            name: step.name,
            kind: StepKind::parse(&step.kind).unwrap_or(StepKind::Step),
            status: StepStatus::parse(&step.status).unwrap_or(StepStatus::Pending),
            attempts: step.attempts,
            has_result: step.result_encrypted.is_some() || step.result_blob_key.is_some(),
            result_bytes: step.result_bytes,
            has_error: step.error_encrypted.is_some(),
            wake_at: step.wake_at,
            duration_ms: step.duration_ms,
            trace_id: step.trace_id,
            error_ref: step.error_ref,
            event_id: step.event_id,
            created_at: step.created_at,
            updated_at: step.updated_at,
            completed_at: step.completed_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowStepListResponse {
    pub steps: Vec<WorkflowStepResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartWorkflowRunRequest {
    pub workflow: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub input: Option<Value>,
    /// A second start with the same key returns the first run.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartWorkflowRunResponse {
    pub run: WorkflowRunResponse,
    /// False when the idempotency key matched an earlier run.
    pub created: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RetryWorkflowRunRequest {
    /// Journal entry to run again from; entries from it on are dropped.
    /// Defaults to the entry that failed, or the end of the journal.
    #[serde(default)]
    pub from_sequence: Option<i32>,
}

/// Which payload to reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WorkflowRevealTarget)]
pub enum RevealTarget {
    Input,
    Output,
    Error,
    StepResult,
    StepError,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowRevealPayloadRequest)]
pub struct RevealPayloadRequest {
    pub target: RevealTarget,
    /// Required for `step_result` and `step_error`.
    #[serde(default)]
    pub sequence: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WorkflowRevealPayloadResponse)]
pub struct RevealPayloadResponse {
    pub target: RevealTarget,
    pub sequence: Option<i32>,
    /// The decrypted payload; `null` when none was stored.
    #[schema(value_type = Object)]
    pub value: Option<Value>,
}

// ── Events ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SendWorkflowEventRequest {
    pub name: String,
    /// Narrows delivery to runs waiting on this key.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SendWorkflowEventResponse {
    pub event_id: i64,
    /// Runs woken by this event.
    pub delivered: u64,
}

// ── Credentials ──────────────────────────────────────────────────────

/// A new workflow credential. `token` is shown this once.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IssuedWorkflowCredentialResponse {
    pub token: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    /// How the app uses it.
    pub usage: String,
}

// ── App-facing API ───────────────────────────────────────────────────

/// A run as the app that started it sees it: its result once finished.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppWorkflowRunResponse {
    pub id: Uuid,
    pub workflow: String,
    pub status: RunStatus,
    pub error_kind: Option<String>,
    /// Set once the run completed.
    #[schema(value_type = Object)]
    pub output: Option<Value>,
    /// Set once the run failed.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppStartWorkflowRunResponse {
    pub run: AppWorkflowRunResponse,
    pub created: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct JournalQuery {
    /// First sequence to return. Defaults to 0.
    pub from: Option<i32>,
}
