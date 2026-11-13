// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storage of workflow state (ADR-103 §5).
//!
//! [`WorkflowStore`] has two parts. The **scheduling** part (runs, leases,
//! claims, waits, events, settings) always lives in the control-plane
//! Postgres. The **journal** part (step entries) is in the same database by
//! default; the opt-in managed journal (M3) will implement only that part
//! elsewhere, writing the journal entry first and the run state second.
//!
//! Every write that changes a running run is fenced by its lease token: a
//! write carrying an older token than the run's is rejected, so a replica that
//! lost its lease (network split, a slow container) cannot change the run.

mod postgres;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use temps_entities::{workflow_credentials, workflow_events, workflow_runs, workflow_steps};
use uuid::Uuid;

use crate::error::WorkflowResult;
use crate::payload::StoredPayload;
use crate::types::{RunStatus, StepKind, StepStatus, WorkflowSettings};

pub use postgres::{PgWorkflowStore, DUE_CHANNEL};

pub type RunRow = workflow_runs::Model;
pub type StepRow = workflow_steps::Model;
pub type EventRow = workflow_events::Model;
pub type CredentialRow = workflow_credentials::Model;

/// Who started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Starter {
    User(i32),
    Credential(i32),
    /// An API key or token with no user.
    Machine,
}

#[derive(Debug, Clone)]
pub struct NewRun {
    pub id: Uuid,
    pub project_id: i32,
    pub environment_id: i32,
    pub workflow_name: String,
    pub input: StoredPayload,
    pub idempotency_key: Option<String>,
    pub deployment_id: Option<i32>,
    pub starter: Starter,
}

#[derive(Debug, Clone)]
pub enum StartOutcome {
    Created(RunRow),
    /// The idempotency key matched this earlier run; nothing was inserted.
    Existing(RunRow),
}

/// A run the scheduler leased, with the status it was claimed from.
#[derive(Debug, Clone)]
pub struct ClaimedRun {
    pub run: RunRow,
    pub previous_status: RunStatus,
}

/// A per-workflow concurrency limit from the environment's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLimit {
    pub environment_id: i32,
    pub workflow_name: String,
    pub limit: u32,
}

/// A run whose lease expired, after the reaper requeued or parked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedRun {
    pub run_id: Uuid,
    pub project_id: i32,
    pub environment_id: i32,
    pub workflow_name: String,
    pub status: RunStatus,
}

/// How an invocation released its lease.
#[derive(Debug, Clone)]
pub enum Release {
    Completed {
        output: StoredPayload,
    },
    Failed {
        error: StoredPayload,
        kind: &'static str,
    },
    Sleep {
        until: DateTime<Utc>,
    },
    Wait {
        sequence: i32,
        event: String,
        key: String,
        timeout_at: Option<DateTime<Utc>>,
    },
    Retry {
        at: DateTime<Utc>,
    },
}

/// One journal write, payloads already sealed.
#[derive(Debug, Clone)]
pub struct StepRecord {
    pub run_id: Uuid,
    pub lease: i64,
    pub sequence: i32,
    pub name: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub attempts: i32,
    pub result: StoredPayload,
    pub error: StoredPayload,
    pub wake_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub trace_id: Option<String>,
    pub error_ref: Option<String>,
    /// For `send_event`: inserted in the same transaction as the entry, so the
    /// event exists exactly when the entry does.
    pub event: Option<NewEvent>,
}

#[derive(Debug, Clone, Copy)]
pub struct StepLimits {
    pub max_steps: i32,
    pub max_journal_bytes: i64,
    pub lease_secs: i32,
}

#[derive(Debug, Clone)]
pub enum StepRecordOutcome {
    Recorded {
        entry: StepRow,
        lease_expires_at: DateTime<Utc>,
        event_id: Option<i64>,
    },
    /// The entry existed with a final status; this is the recorded one.
    AlreadyRecorded {
        entry: StepRow,
        lease_expires_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub project_id: i32,
    pub environment_id: i32,
    pub name: String,
    pub key: String,
    pub payload: StoredPayload,
    /// `api`, `credential` or `workflow`.
    pub source: &'static str,
    pub source_run_id: Option<Uuid>,
}

/// A run suspended on an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingRun {
    pub run_id: Uuid,
    pub project_id: i32,
    pub environment_id: i32,
    pub sequence: i32,
    pub event_name: String,
    pub event_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueStat {
    pub project_id: i32,
    pub environment_id: i32,
    pub workflow_name: String,
    pub depth: i64,
    pub oldest_due_at: Option<DateTime<Utc>>,
    pub stuck: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSize {
    pub project_id: i32,
    pub environment_id: i32,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredRun {
    pub run_id: Uuid,
    pub project_id: i32,
    pub environment_id: i32,
    pub has_blob_payloads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSummary {
    pub workflow_name: String,
    pub active: i64,
    pub completed: i64,
    pub failed: i64,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct RunFilter {
    pub status: Option<RunStatus>,
    pub workflow_name: Option<String>,
}

#[async_trait]
pub trait WorkflowStore: Send + Sync {
    // ── Environments, settings, credentials ──────────────────────────

    /// The project an environment belongs to, if it exists.
    async fn environment_project(&self, environment_id: i32) -> WorkflowResult<Option<i32>>;
    async fn settings(&self, environment_id: i32) -> WorkflowResult<WorkflowSettings>;
    async fn upsert_settings(
        &self,
        project_id: i32,
        environment_id: i32,
        settings: &WorkflowSettings,
    ) -> WorkflowResult<WorkflowSettings>;
    async fn active_credential(&self, environment_id: i32)
        -> WorkflowResult<Option<CredentialRow>>;
    /// Revoke the active credential (if any) and store a new one, atomically.
    async fn rotate_credential(
        &self,
        project_id: i32,
        environment_id: i32,
        hash: &str,
        prefix: &str,
        user_id: Option<i32>,
    ) -> WorkflowResult<CredentialRow>;
    async fn revoke_credential(&self, environment_id: i32) -> WorkflowResult<bool>;
    /// The active credential with this hash; touches `last_used_at` at most
    /// once a minute.
    async fn credential_by_hash(&self, hash: &str) -> WorkflowResult<Option<CredentialRow>>;

    // ── Scheduling ───────────────────────────────────────────────────

    /// Insert a run, honouring its idempotency key and `max_active_runs`.
    async fn insert_run(&self, run: NewRun, max_active_runs: i64) -> WorkflowResult<StartOutcome>;
    /// Lease up to `batch` due runs, oldest first, never over an
    /// environment's or a workflow's concurrency limit. Serialized across
    /// control-plane processes; rows locked elsewhere are skipped.
    async fn claim_due(
        &self,
        batch: u64,
        limits: &[WorkflowLimit],
    ) -> WorkflowResult<Vec<ClaimedRun>>;
    /// Runs of `workflow_name` in the environment holding a lease, other than
    /// `except`: the check a claim made without the workflow's concurrency
    /// limit (its manifest was not cached yet) repeats before invoking.
    async fn running_count(
        &self,
        environment_id: i32,
        workflow_name: &str,
        except: Uuid,
    ) -> WorkflowResult<i64>;
    /// Give a claimed run back without counting an attempt (nothing could be
    /// invoked), due again at `retry_at`, with the reason shown to users.
    async fn block_claimed(
        &self,
        run_id: Uuid,
        lease: i64,
        previous_status: RunStatus,
        reason: &str,
        retry_at: DateTime<Utc>,
    ) -> WorkflowResult<bool>;
    /// An invocation failed: retry with backoff, or park the run as `stuck`
    /// after the environment's `stuck_after_failures`. `None` when the lease
    /// was no longer held.
    async fn fail_invocation(
        &self,
        run_id: Uuid,
        lease: i64,
        reason: &str,
    ) -> WorkflowResult<Option<RunStatus>>;
    /// Extend the lease; returns its new expiry.
    async fn heartbeat(&self, run_id: Uuid, lease: i64) -> WorkflowResult<DateTime<Utc>>;
    /// End an invocation; fenced by `lease`.
    async fn release(&self, run_id: Uuid, lease: i64, release: Release) -> WorkflowResult<RunRow>;
    /// Requeue (or park as stuck) up to `batch` runs whose lease expired.
    async fn reap_expired(&self, batch: u64) -> WorkflowResult<Vec<ReapedRun>>;
    /// Cancel a non-terminal run. `None` when it was already terminal. The
    /// lease token moves on, so an in-flight invocation's writes are rejected.
    async fn cancel_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>>;
    /// Drop the journal from `sequence` on and run again from there. Only a
    /// terminal or stuck run; `None` otherwise.
    async fn retry_from(&self, run_id: Uuid, sequence: i32) -> WorkflowResult<Option<RunRow>>;
    /// Make a stuck, retrying, sleeping or blocked run due now (a pending sleep
    /// wakes now). `None` for any other status.
    async fn resume_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>>;
    /// Delete a run that is not running, with its journal.
    async fn delete_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>>;
    /// Wake every listener (`NOTIFY workflow_runs_due`).
    async fn notify_due(&self) -> WorkflowResult<()>;

    // ── Events ───────────────────────────────────────────────────────

    async fn insert_event(&self, event: NewEvent) -> WorkflowResult<i64>;
    async fn event(&self, event_id: i64) -> WorkflowResult<Option<EventRow>>;
    /// Runs of the environment waiting on `name`/`key`, after `after`.
    async fn waiting_runs(
        &self,
        environment_id: i32,
        name: &str,
        key: &str,
        after: Option<Uuid>,
        limit: u64,
    ) -> WorkflowResult<Vec<WaitingRun>>;
    /// Waiting runs with a matching event they have not consumed: the
    /// reconciliation of deliveries a crash interrupted.
    async fn undelivered_waits(&self, limit: u64) -> WorkflowResult<Vec<WaitingRun>>;
    /// The oldest event matching a run's wait that the run has not consumed,
    /// received since the run started.
    async fn buffered_event(
        &self,
        run_id: Uuid,
        name: &str,
        key: &str,
    ) -> WorkflowResult<Option<EventRow>>;
    /// Complete a pending `wait` entry and make its run due. Compare-and-set:
    /// false when the entry was no longer pending.
    async fn complete_wait(
        &self,
        run_id: Uuid,
        sequence: i32,
        event_id: Option<i64>,
        result: StoredPayload,
    ) -> WorkflowResult<bool>;

    // ── Journal ──────────────────────────────────────────────────────

    async fn record_step(
        &self,
        record: StepRecord,
        limits: StepLimits,
    ) -> WorkflowResult<StepRecordOutcome>;
    async fn journal(
        &self,
        run_id: Uuid,
        from_sequence: i32,
        limit: u64,
    ) -> WorkflowResult<Vec<StepRow>>;
    async fn step(&self, run_id: Uuid, sequence: i32) -> WorkflowResult<Option<StepRow>>;

    // ── Reads, metrics, retention ────────────────────────────────────

    async fn get_run(&self, run_id: Uuid) -> WorkflowResult<Option<RunRow>>;
    async fn list_runs(
        &self,
        environment_id: i32,
        filter: &RunFilter,
        page: u64,
        page_size: u64,
    ) -> WorkflowResult<(Vec<RunRow>, u64)>;
    async fn workflow_summaries(
        &self,
        environment_id: i32,
    ) -> WorkflowResult<HashMap<String, WorkflowSummary>>;
    async fn queue_stats(&self) -> WorkflowResult<Vec<QueueStat>>;
    async fn journal_sizes(&self) -> WorkflowResult<Vec<JournalSize>>;
    /// Delete up to `batch` finished runs past their environment's retention
    /// and return them, so their blobs can be removed afterwards.
    async fn delete_expired_runs(&self, batch: u64) -> WorkflowResult<Vec<ExpiredRun>>;
    async fn delete_expired_events(&self, batch: u64) -> WorkflowResult<u64>;
}
