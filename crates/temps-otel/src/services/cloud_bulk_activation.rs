// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The durable state of a bulk Cloud-telemetry activation job (ADR-042 §7, §8).
//!
//! Everything the [`CloudBulkActivationWorker`](super::cloud_bulk_activation_worker)
//! knows about a job it is running lives in these two tables and is read and
//! written only through this service. That is not architectural tidiness for
//! its own sake: the worker has no HTTP request to fail back to and no operator
//! watching a terminal, so "where did this job get to" has to be answerable
//! from the database alone, by a process that did not start it.
//!
//! # The one invariant worth restating
//!
//! **At most one job may be `pending` or `running`.** Submission concurrency is
//! 1 globally (ADR-041 §3b), so a second job would not run twice as fast — it
//! would contend for a submission scope that is exclusive process-wide and fail
//! in a way that costs money to diagnose. [`Self::enqueue_job`] therefore
//! refuses with [`CloudBulkActivationError::JobAlreadyActive`], which carries
//! the in-flight job's id so a caller can redirect to it rather than reporting
//! a bare conflict. A partial unique index enforces the same thing underneath,
//! so a race between two callers loses cleanly instead of both winning.
//!
//! # Why totals are recomputed rather than incremented
//!
//! A job's `spans_shipped`/`bytes_shipped` are rewritten from `SUM()` over its
//! project rows on every progress write. Incrementing would be one fewer row
//! touched and would drift the first time a chunk write were retried after a
//! partial failure — and the number it drifts on is the one a customer reads
//! against an invoice.

use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend,
    DatabaseConnection, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    Statement, TransactionTrait, Unchanged,
};
use temps_core::DBDateTime;
use temps_entities::cloud_telemetry_bulk_job_projects::{
    self as job_projects, BulkJobProjectStatus, Model as BulkJobProject,
};
use temps_entities::cloud_telemetry_bulk_jobs::{
    self as bulk_jobs, BulkJobStatus, BulkJobTrigger, Model as BulkJob,
};
use uuid::Uuid;

use crate::services::cloud_backfill::CloudBackfillCursor;
use crate::services::cloud_backfill_progress::truncate_failure_reason;
use crate::services::telemetry_write_mode::CLOUD_SETUP_PATH;

/// Default page size for job listings, per the repository pagination rule.
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// Hard ceiling on a requested page size.
pub const MAX_PAGE_SIZE: u64 = 100;

/// Why a project was never switched, in a form a client can branch on.
///
/// A skip is a decision the job refused to make on the operator's behalf, so it
/// always carries both a reason and the page that resolves it (ADR-042 §4).
/// Raising a project's Cloud telemetry fidelity costs money and changes what
/// leaves the instance; it must never happen as an invisible consequence of
/// paying for Temps Cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkSkipReason {
    /// `cloud_telemetry_fidelity` is not `queryable`, so a Cloud-primary switch
    /// would store unreadable placeholders in Cloud and nothing locally.
    FidelityNotQueryable,
    /// The project was deleted between the job being planned and its turn
    /// arriving. Not an error: the row deliberately has no foreign key so a
    /// deletion is never blocked by telemetry bookkeeping.
    ProjectNotFound,
}

impl BulkSkipReason {
    /// The stored, machine-readable token.
    pub fn as_str(&self) -> &'static str {
        match self {
            BulkSkipReason::FidelityNotQueryable => "fidelity_not_queryable",
            BulkSkipReason::ProjectNotFound => "project_not_found",
        }
    }

    /// Where an operator goes to unblock this project, when anywhere.
    pub fn setup_path(&self, project_id: i32) -> Option<String> {
        match self {
            BulkSkipReason::FidelityNotQueryable => {
                Some(format!("/projects/{project_id}/settings/telemetry"))
            }
            BulkSkipReason::ProjectNotFound => None,
        }
    }
}

impl std::fmt::Display for BulkSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an instance-wide condition stopped a whole job (ADR-042 §7).
///
/// These are properties of the Cloud link, not of project 17. Continuing would
/// fail the remaining projects identically and bury the one real cause under a
/// pile of duplicates, so the job stops with a single actionable reason and its
/// untouched projects stay `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAbortReason {
    /// The instance is not linked to Temps Cloud.
    NotLinked,
    /// Temps Cloud refused this instance's credential.
    CredentialRejected,
    /// The operator switched Cloud telemetry export off.
    TelemetryExportDisabled,
    /// Something else already holds the instance's single submission scope —
    /// the offline `temps backfill cloud-telemetry` tool, most plausibly. Also
    /// instance-wide: the claim is process-global, so every remaining project
    /// would hit it too.
    SubmissionScopeBusy,
}

impl BulkAbortReason {
    /// The stored, machine-readable token.
    pub fn as_str(&self) -> &'static str {
        match self {
            BulkAbortReason::NotLinked => "not_linked",
            BulkAbortReason::CredentialRejected => "credential_rejected",
            BulkAbortReason::TelemetryExportDisabled => "telemetry_export_disabled",
            BulkAbortReason::SubmissionScopeBusy => "submission_scope_busy",
        }
    }

    /// What the operator has to do about it, and where.
    ///
    /// Written out rather than left to the client, because the same sentence
    /// has to be readable from the Console, the CLI and a log line — and a
    /// self-hosted operator has nobody to ask what `credential_rejected` meant.
    pub fn detail(&self) -> String {
        match self {
            BulkAbortReason::NotLinked => format!(
                "This instance is no longer linked to Temps Cloud, so there is nowhere to \
                 activate projects to. Link it again at {CLOUD_SETUP_PATH}, then resume this job."
            ),
            BulkAbortReason::CredentialRejected => format!(
                "Temps Cloud rejected this instance's credential, so nothing can be shipped. \
                 Re-enroll the instance at {CLOUD_SETUP_PATH}, then resume this job."
            ),
            BulkAbortReason::TelemetryExportDisabled => format!(
                "Temps Cloud telemetry export is switched off for this instance, so no span \
                 would leave. Turn it on at {CLOUD_SETUP_PATH}, then resume this job."
            ),
            BulkAbortReason::SubmissionScopeBusy => {
                "Another Temps Cloud submission already holds this instance's submission scope \
                 — most likely an `temps backfill cloud-telemetry` run started from a shell. \
                 Only one may be in flight at a time. Let it finish, then resume this job."
                    .to_string()
            }
        }
    }
}

impl std::fmt::Display for BulkAbortReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CloudBulkActivationError {
    #[error(
        "A bulk Temps Cloud telemetry activation job is already {status} on this instance \
         (job {job_id}). Only one may run at a time, because this instance may have exactly \
         one Temps Cloud submission in flight. Watch or cancel that job instead of starting \
         a second one."
    )]
    JobAlreadyActive { job_id: Uuid, status: BulkJobStatus },

    #[error(
        "A bulk Temps Cloud telemetry activation job must name at least one project. This one \
         named none, so there would be nothing to switch and nothing to ship."
    )]
    NoProjects,

    #[error(
        "Project {project_id} appears more than once in the same bulk Temps Cloud telemetry \
         activation job. Each project may be activated once per job; shipping the same window \
         twice would bill the same history twice."
    )]
    DuplicateProject { project_id: i32 },

    #[error(
        "Project {project_id} was given the backfill window [{from}, {to}], which ends before \
         it starts. Refusing to plan a job that could never ship anything for it."
    )]
    InvalidWindow {
        project_id: i32,
        from: String,
        to: String,
    },

    #[error(
        "Bulk Temps Cloud telemetry activation job {job_id} does not exist on this instance. \
         It may have been created against a different database, or the id may be mistyped."
    )]
    JobNotFound { job_id: Uuid },

    #[error(
        "Bulk Temps Cloud telemetry activation job {job_id} has no row for project \
         {project_id}, so there is nothing to record against it."
    )]
    JobProjectNotFound { job_id: Uuid, project_id: i32 },

    #[error(
        "Failed to {operation} for bulk Temps Cloud telemetry activation job {job_id}: {source}"
    )]
    Job {
        job_id: Uuid,
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Failed to {operation} for bulk Temps Cloud telemetry activation jobs: {source}")]
    Store {
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

/// One project's place in a job, as the caller plans it.
///
/// The window is fixed at enqueue time and never recomputed. "Everything local
/// storage holds" is a moving target — recomputing it after a restart would
/// silently change what the customer authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkJobProjectPlan {
    pub project_id: i32,
    pub window_from: DBDateTime,
    pub window_to: DBDateTime,
    /// Pre-send estimate, from `estimate_backfill`. Zero when the caller has
    /// not estimated — the job still runs and still records actuals.
    pub estimated_spans: u64,
    pub estimated_bytes: u64,
}

/// Everything needed to create a job.
///
/// The plain-Rust entry point ADR-042 P2 (operator path) and P3 (purchase path)
/// each call from their own trigger. Neither exists yet; this is deliberately
/// the whole seam, so adding them is adding a caller rather than reworking the
/// engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueBulkJobRequest {
    pub trigger: BulkJobTrigger,
    /// `None` on the purchase path: the payment is the authorization and there
    /// is no operator to attribute the spend to.
    pub requested_by_user_id: Option<i32>,
    /// Binds an operator-path job to the exact estimate that was confirmed.
    pub plan_hash: Option<String>,
    pub projects: Vec<BulkJobProjectPlan>,
}

/// A job together with its project rows, in processing order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkJobDetail {
    pub job: BulkJob,
    /// Ascending `project_id` — the documented processing order (ADR-042 §3),
    /// so what the Console lists is the order work will actually happen in.
    pub projects: Vec<BulkJobProject>,
}

impl BulkJobDetail {
    /// Projects that still need the worker.
    pub fn pending_projects(&self) -> usize {
        self.projects
            .iter()
            .filter(|project| project.status.is_pending())
            .count()
    }

    /// Whether every project has reached a terminal state.
    pub fn is_finished(&self) -> bool {
        self.pending_projects() == 0
    }
}

/// Reads and writes the bulk activation job tables.
pub struct CloudBulkActivationService {
    db: Arc<DatabaseConnection>,
}

impl CloudBulkActivationService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    // ── Creation and cancellation (the P2/P3 seam) ───────────────────────

    /// Create a job and its project rows in one transaction.
    ///
    /// Refuses while another job is `pending` or `running`, naming that job's
    /// id so the caller can point at it rather than reporting a bare conflict.
    pub async fn enqueue_job(
        &self,
        request: EnqueueBulkJobRequest,
    ) -> Result<BulkJobDetail, CloudBulkActivationError> {
        if request.projects.is_empty() {
            return Err(CloudBulkActivationError::NoProjects);
        }

        let mut seen = std::collections::BTreeSet::new();
        for plan in &request.projects {
            if plan.window_to < plan.window_from {
                return Err(CloudBulkActivationError::InvalidWindow {
                    project_id: plan.project_id,
                    from: plan.window_from.to_rfc3339(),
                    to: plan.window_to.to_rfc3339(),
                });
            }
            if !seen.insert(plan.project_id) {
                return Err(CloudBulkActivationError::DuplicateProject {
                    project_id: plan.project_id,
                });
            }
        }

        // Checked here for the good error message, and again by the partial
        // unique index underneath for the race. Both are needed: the check
        // alone loses a race, the index alone reports a constraint name.
        if let Some(active) = self.active_job().await? {
            return Err(CloudBulkActivationError::JobAlreadyActive {
                job_id: active.job.id,
                status: active.job.status,
            });
        }

        let job_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let estimated_spans = request.projects.iter().fold(0i64, |total, plan| {
            total.saturating_add(clamp_to_i64(plan.estimated_spans))
        });
        let estimated_bytes = request.projects.iter().fold(0i64, |total, plan| {
            total.saturating_add(clamp_to_i64(plan.estimated_bytes))
        });

        let transaction =
            self.db
                .begin()
                .await
                .map_err(|source| CloudBulkActivationError::Store {
                    operation: "open the transaction that creates a job",
                    source,
                })?;

        let insert = bulk_jobs::ActiveModel {
            id: Set(job_id),
            trigger: Set(request.trigger),
            requested_by_user_id: Set(request.requested_by_user_id),
            status: Set(BulkJobStatus::Pending),
            estimated_spans: Set(estimated_spans),
            estimated_bytes: Set(estimated_bytes),
            spans_shipped: Set(0),
            bytes_shipped: Set(0),
            plan_hash: Set(request.plan_hash.clone()),
            created_at: Set(now),
            started_at: Set(None),
            completed_at: Set(None),
            cancel_requested_at: Set(None),
            abort_reason: Set(None),
        }
        .insert(&transaction)
        .await;

        let job = match insert {
            Ok(job) => job,
            Err(source) => {
                // Lost the race against a concurrent enqueue. Report the winner
                // rather than a constraint name — the caller's next move is to
                // watch that job, and it cannot do that from a `DbErr`.
                let _ = transaction.rollback().await;
                if let Some(active) = self.active_job().await? {
                    return Err(CloudBulkActivationError::JobAlreadyActive {
                        job_id: active.job.id,
                        status: active.job.status,
                    });
                }
                return Err(CloudBulkActivationError::Job {
                    job_id,
                    operation: "insert the job row",
                    source,
                });
            }
        };

        for plan in &request.projects {
            job_projects::ActiveModel {
                job_id: Set(job_id),
                project_id: Set(plan.project_id),
                status: Set(BulkJobProjectStatus::Pending),
                skip_reason: Set(None),
                window_from: Set(plan.window_from),
                window_to: Set(plan.window_to),
                estimated_spans: Set(clamp_to_i64(plan.estimated_spans)),
                estimated_bytes: Set(clamp_to_i64(plan.estimated_bytes)),
                spans_shipped: Set(0),
                bytes_shipped: Set(0),
                resume_start_time: Set(None),
                resume_row_id: Set(None),
                resume_span_id: Set(None),
                last_error: Set(None),
                started_at: Set(None),
                completed_at: Set(None),
                ..Default::default()
            }
            .insert(&transaction)
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "insert a project row",
                source,
            })?;
        }

        transaction
            .commit()
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "commit the job",
                source,
            })?;

        tracing::info!(
            %job_id,
            trigger = %request.trigger,
            projects = request.projects.len(),
            estimated_spans,
            estimated_bytes,
            "Queued a bulk Temps Cloud telemetry activation job"
        );

        Ok(BulkJobDetail {
            job,
            projects: self.projects_of(job_id).await?,
        })
    }

    /// Ask a job to stop.
    ///
    /// Only records the request; the worker honours it at the next chunk
    /// boundary, where the cursor is durable and stopping is lossless. A job
    /// that has already finished is returned unchanged rather than treated as
    /// an error — "cancel something that just completed" is a race a UI will
    /// lose regularly, and failing it would be noise.
    pub async fn request_cancel(&self, job_id: Uuid) -> Result<BulkJob, CloudBulkActivationError> {
        let job = self.job(job_id).await?;
        if job.status.is_terminal() || job.cancel_requested_at.is_some() {
            return Ok(job);
        }

        let updated = bulk_jobs::ActiveModel {
            id: Unchanged(job.id),
            cancel_requested_at: Set(Some(chrono::Utc::now())),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "record the cancellation request",
            source,
        })?;

        tracing::info!(%job_id, "Cancellation requested for a bulk Temps Cloud activation job");
        Ok(updated)
    }

    // ── Reads ────────────────────────────────────────────────────────────

    /// The job the worker should be running, if any.
    pub async fn active_job(&self) -> Result<Option<BulkJobDetail>, CloudBulkActivationError> {
        let job = bulk_jobs::Entity::find()
            .filter(
                bulk_jobs::Column::Status.is_in([BulkJobStatus::Running, BulkJobStatus::Pending]),
            )
            // `running` before `pending` so a resumed job is preferred over a
            // freshly queued one. The unique index makes two impossible, but a
            // deterministic order costs nothing and stops a future relaxation
            // of that index from silently reordering work.
            .order_by_desc(bulk_jobs::Column::Status)
            .order_by_asc(bulk_jobs::Column::CreatedAt)
            .one(self.db.as_ref())
            .await
            .map_err(|source| CloudBulkActivationError::Store {
                operation: "find the active job",
                source,
            })?;

        match job {
            Some(job) => {
                let projects = self.projects_of(job.id).await?;
                Ok(Some(BulkJobDetail { job, projects }))
            }
            None => Ok(None),
        }
    }

    /// One job row.
    pub async fn job(&self, job_id: Uuid) -> Result<BulkJob, CloudBulkActivationError> {
        bulk_jobs::Entity::find_by_id(job_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "read the job",
                source,
            })?
            .ok_or(CloudBulkActivationError::JobNotFound { job_id })
    }

    /// One job with its project rows.
    pub async fn job_detail(
        &self,
        job_id: Uuid,
    ) -> Result<BulkJobDetail, CloudBulkActivationError> {
        let job = self.job(job_id).await?;
        let projects = self.projects_of(job_id).await?;
        Ok(BulkJobDetail { job, projects })
    }

    /// A job's project rows, in processing order.
    pub async fn projects_of(
        &self,
        job_id: Uuid,
    ) -> Result<Vec<BulkJobProject>, CloudBulkActivationError> {
        job_projects::Entity::find()
            .filter(job_projects::Column::JobId.eq(job_id))
            .order_by_asc(job_projects::Column::ProjectId)
            .all(self.db.as_ref())
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "read the project rows",
                source,
            })
    }

    /// Job history, newest first.
    pub async fn list_jobs(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<BulkJob>, u64), CloudBulkActivationError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let paginator = bulk_jobs::Entity::find()
            .order_by_desc(bulk_jobs::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total =
            paginator
                .num_items()
                .await
                .map_err(|source| CloudBulkActivationError::Store {
                    operation: "count jobs",
                    source,
                })?;
        let items = paginator.fetch_page(page - 1).await.map_err(|source| {
            CloudBulkActivationError::Store {
                operation: "read a page of jobs",
                source,
            }
        })?;
        Ok((items, total))
    }

    /// Whether a cancellation has been requested. Read at every chunk boundary,
    /// so it is deliberately one narrow column rather than the whole row.
    pub async fn cancel_requested(&self, job_id: Uuid) -> Result<bool, CloudBulkActivationError> {
        let requested = bulk_jobs::Entity::find_by_id(job_id)
            .select_only()
            .column(bulk_jobs::Column::CancelRequestedAt)
            .into_tuple::<Option<DBDateTime>>()
            .one(self.db.as_ref())
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "read the cancellation flag",
                source,
            })?;
        Ok(requested.flatten().is_some())
    }

    /// The lowest-numbered project in this job that still needs work.
    ///
    /// `switching` and `backfilling` count as needing work: a process killed
    /// mid-project left them there, and its cursor is what resumes them.
    pub async fn next_pending_project(
        &self,
        job_id: Uuid,
    ) -> Result<Option<BulkJobProject>, CloudBulkActivationError> {
        job_projects::Entity::find()
            .filter(job_projects::Column::JobId.eq(job_id))
            .filter(job_projects::Column::Status.is_in([
                BulkJobProjectStatus::Pending,
                BulkJobProjectStatus::Switching,
                BulkJobProjectStatus::Backfilling,
            ]))
            .order_by_asc(job_projects::Column::ProjectId)
            .one(self.db.as_ref())
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "find the next pending project",
                source,
            })
    }

    // ── Worker state transitions ─────────────────────────────────────────

    /// Move a job to `running`, stamping `started_at` on the first pass only.
    pub async fn mark_job_running(
        &self,
        job_id: Uuid,
    ) -> Result<BulkJob, CloudBulkActivationError> {
        let job = self.job(job_id).await?;
        if job.status == BulkJobStatus::Running {
            return Ok(job);
        }
        bulk_jobs::ActiveModel {
            id: Unchanged(job.id),
            status: Set(BulkJobStatus::Running),
            // Preserved across a restart: `started_at` is when the activation
            // began for the customer, not when this process picked it up.
            started_at: Set(job.started_at.or_else(|| Some(chrono::Utc::now()))),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "mark the job running",
            source,
        })
    }

    /// Move a project into `switching`, stamping `started_at` once.
    pub async fn mark_project_switching(
        &self,
        job_id: Uuid,
        project_id: i32,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        let row = self.project_row(job_id, project_id).await?;
        job_projects::ActiveModel {
            id: Unchanged(row.id),
            status: Set(BulkJobProjectStatus::Switching),
            started_at: Set(row.started_at.or_else(|| Some(chrono::Utc::now()))),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "mark a project switching",
            source,
        })
    }

    /// Move a project into `backfilling`. The switch has landed by this point
    /// and is never rolled back (ADR-042 §7).
    pub async fn mark_project_backfilling(
        &self,
        job_id: Uuid,
        project_id: i32,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        let row = self.project_row(job_id, project_id).await?;
        job_projects::ActiveModel {
            id: Unchanged(row.id),
            status: Set(BulkJobProjectStatus::Backfilling),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "mark a project backfilling",
            source,
        })
    }

    /// Persist a completed chunk: the resume cursor and the absolute totals.
    ///
    /// Absolute rather than incremental, so replaying this write after a crash
    /// cannot double-count. The job's totals are then recomputed as the sum of
    /// its project rows, which makes drift between the two structurally
    /// impossible rather than merely unlikely.
    pub async fn record_project_progress(
        &self,
        job_id: Uuid,
        project_id: i32,
        cursor: &CloudBackfillCursor,
        spans_shipped: u64,
        bytes_shipped: u64,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        let row = self.project_row(job_id, project_id).await?;

        let transaction =
            self.db
                .begin()
                .await
                .map_err(|source| CloudBulkActivationError::Job {
                    job_id,
                    operation: "open the transaction that records chunk progress",
                    source,
                })?;

        let updated = job_projects::ActiveModel {
            id: Unchanged(row.id),
            spans_shipped: Set(clamp_to_i64(spans_shipped)),
            bytes_shipped: Set(clamp_to_i64(bytes_shipped)),
            resume_start_time: Set(cursor.last_start_time),
            resume_row_id: Set(cursor.last_row_id),
            resume_span_id: Set(cursor.last_span_id.clone()),
            ..Default::default()
        }
        .update(&transaction)
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "record chunk progress for a project",
            source,
        })?;

        recompute_job_totals(&transaction, job_id).await?;

        transaction
            .commit()
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "commit chunk progress",
                source,
            })?;

        Ok(updated)
    }

    /// Mark a project fully activated.
    pub async fn mark_project_done(
        &self,
        job_id: Uuid,
        project_id: i32,
        spans_shipped: u64,
        bytes_shipped: u64,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        self.settle_project(
            job_id,
            project_id,
            BulkJobProjectStatus::Done,
            Some(spans_shipped),
            Some(bytes_shipped),
            None,
            None,
        )
        .await
    }

    /// Mark a project failed, keeping the reason bounded.
    ///
    /// The mode switch that already happened is **not** reverted. Reverting
    /// after some spans shipped Cloud-primary would split the project's history
    /// across both stores and write a false boundary into the interval ledger;
    /// a recorded, retryable hole is honest, a silently bisected timeline is
    /// not (ADR-042 §7).
    pub async fn mark_project_failed(
        &self,
        job_id: Uuid,
        project_id: i32,
        spans_shipped: u64,
        bytes_shipped: u64,
        reason: impl AsRef<str>,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        self.settle_project(
            job_id,
            project_id,
            BulkJobProjectStatus::Failed,
            Some(spans_shipped),
            Some(bytes_shipped),
            Some(truncate_failure_reason(reason.as_ref())),
            None,
        )
        .await
    }

    /// Mark a project skipped, with the reason that unblocks it.
    pub async fn mark_project_skipped(
        &self,
        job_id: Uuid,
        project_id: i32,
        reason: BulkSkipReason,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        self.settle_project(
            job_id,
            project_id,
            BulkJobProjectStatus::Skipped,
            None,
            None,
            None,
            Some(reason),
        )
        .await
    }

    /// Hand a project back to `pending`, keeping its cursor.
    ///
    /// Used when the *job* stops for a reason that is not this project's fault
    /// — an instance-wide abort or a cancellation. Marking it `failed` would
    /// blame it for the link being down and would need an operator to
    /// distinguish the two before retrying; leaving it `pending` with its
    /// cursor intact means a resume continues from exactly where it stopped and
    /// re-ships nothing.
    pub async fn release_project_to_pending(
        &self,
        job_id: Uuid,
        project_id: i32,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        let row = self.project_row(job_id, project_id).await?;
        job_projects::ActiveModel {
            id: Unchanged(row.id),
            status: Set(BulkJobProjectStatus::Pending),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "release a project back to pending",
            source,
        })
    }

    /// Stop the whole job on an instance-wide condition.
    ///
    /// Every project still `pending` stays `pending` on purpose: they were
    /// never attempted, they are not broken, and a resume must not have to
    /// re-authorize them.
    pub async fn abort_job(
        &self,
        job_id: Uuid,
        reason: BulkAbortReason,
    ) -> Result<BulkJob, CloudBulkActivationError> {
        let stored = format!("{}: {}", reason.as_str(), reason.detail());
        let job = bulk_jobs::ActiveModel {
            id: Unchanged(job_id),
            status: Set(BulkJobStatus::Aborted),
            completed_at: Set(Some(chrono::Utc::now())),
            abort_reason: Set(Some(truncate_failure_reason(&stored))),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "abort the job",
            source,
        })?;

        tracing::error!(
            %job_id,
            reason = reason.as_str(),
            "A bulk Temps Cloud telemetry activation job stopped: {}",
            reason.detail()
        );
        Ok(job)
    }

    /// Settle a job that was asked to stop.
    pub async fn mark_job_cancelled(
        &self,
        job_id: Uuid,
    ) -> Result<BulkJob, CloudBulkActivationError> {
        let job = bulk_jobs::ActiveModel {
            id: Unchanged(job_id),
            status: Set(BulkJobStatus::Cancelled),
            completed_at: Set(Some(chrono::Utc::now())),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "mark the job cancelled",
            source,
        })?;

        tracing::info!(%job_id, "A bulk Temps Cloud telemetry activation job was cancelled");
        Ok(job)
    }

    /// Settle a job whose projects have all reached a terminal state.
    ///
    /// `completed_with_failures` is a separate state from `completed` because a
    /// job that finished with three failed projects needs a retry affordance
    /// and a green checkmark does not.
    pub async fn finish_job(&self, job_id: Uuid) -> Result<BulkJob, CloudBulkActivationError> {
        let projects = self.projects_of(job_id).await?;
        let failed = projects
            .iter()
            .filter(|project| project.status == BulkJobProjectStatus::Failed)
            .count();
        let status = if failed > 0 {
            BulkJobStatus::CompletedWithFailures
        } else {
            BulkJobStatus::Completed
        };

        let job = bulk_jobs::ActiveModel {
            id: Unchanged(job_id),
            status: Set(status),
            completed_at: Set(Some(chrono::Utc::now())),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "finish the job",
            source,
        })?;

        tracing::info!(
            %job_id,
            status = %status,
            projects = projects.len(),
            failed,
            skipped = projects
                .iter()
                .filter(|project| project.status == BulkJobProjectStatus::Skipped)
                .count(),
            spans_shipped = job.spans_shipped,
            "A bulk Temps Cloud telemetry activation job finished"
        );
        Ok(job)
    }

    // ── Internals ────────────────────────────────────────────────────────

    async fn project_row(
        &self,
        job_id: Uuid,
        project_id: i32,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        job_projects::Entity::find()
            .filter(job_projects::Column::JobId.eq(job_id))
            .filter(job_projects::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "read a project row",
                source,
            })?
            .ok_or(CloudBulkActivationError::JobProjectNotFound { job_id, project_id })
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_project(
        &self,
        job_id: Uuid,
        project_id: i32,
        status: BulkJobProjectStatus,
        spans_shipped: Option<u64>,
        bytes_shipped: Option<u64>,
        last_error: Option<String>,
        skip_reason: Option<BulkSkipReason>,
    ) -> Result<BulkJobProject, CloudBulkActivationError> {
        let row = self.project_row(job_id, project_id).await?;

        let transaction =
            self.db
                .begin()
                .await
                .map_err(|source| CloudBulkActivationError::Job {
                    job_id,
                    operation: "open the transaction that settles a project",
                    source,
                })?;

        let mut active = job_projects::ActiveModel {
            id: Unchanged(row.id),
            status: Set(status),
            completed_at: Set(Some(chrono::Utc::now())),
            // A settled project either carries a failure reason or clears the
            // one a previous attempt left, so a `done` row never displays the
            // error from the attempt before it.
            last_error: Set(last_error),
            skip_reason: Set(skip_reason.map(|reason| reason.as_str().to_string())),
            ..Default::default()
        };
        if let Some(spans) = spans_shipped {
            active.spans_shipped = Set(clamp_to_i64(spans));
        }
        if let Some(bytes) = bytes_shipped {
            active.bytes_shipped = Set(clamp_to_i64(bytes));
        }

        let updated =
            active
                .update(&transaction)
                .await
                .map_err(|source| CloudBulkActivationError::Job {
                    job_id,
                    operation: "settle a project",
                    source,
                })?;

        recompute_job_totals(&transaction, job_id).await?;

        transaction
            .commit()
            .await
            .map_err(|source| CloudBulkActivationError::Job {
                job_id,
                operation: "commit a settled project",
                source,
            })?;

        Ok(updated)
    }
}

/// Rewrite a job's totals as the sum of its project rows.
///
/// One statement, inside the caller's transaction, so the job total and the
/// project totals are never observable as disagreeing with each other. That
/// matters more than the extra row touched: this is the number a customer reads
/// against an invoice.
async fn recompute_job_totals<C: ConnectionTrait>(
    connection: &C,
    job_id: Uuid,
) -> Result<(), CloudBulkActivationError> {
    connection
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE cloud_telemetry_bulk_jobs AS j \
             SET spans_shipped = COALESCE(totals.spans, 0), \
                 bytes_shipped = COALESCE(totals.bytes, 0) \
             FROM (SELECT SUM(spans_shipped)::bigint AS spans, \
                          SUM(bytes_shipped)::bigint AS bytes \
                   FROM cloud_telemetry_bulk_job_projects WHERE job_id = $1) AS totals \
             WHERE j.id = $1",
            vec![job_id.into()],
        ))
        .await
        .map_err(|source| CloudBulkActivationError::Job {
            job_id,
            operation: "recompute the job totals",
            source,
        })?;
    Ok(())
}

/// Span/byte counts are `u64` in the backfill and `BIGINT` in Postgres.
/// Saturating rather than wrapping: a nonsensical count should read as "very
/// large", never as a negative that would render as a negative percentage.
fn clamp_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The resume cursor a project row carries, as the backfill wants it.
///
/// A fresh row yields [`CloudBackfillCursor::default`], which
/// `backfill_cloud_telemetry_window` reads as "start at the beginning of the
/// window" — the same value the CLI passes without `--resume`.
pub fn cursor_of(project: &BulkJobProject) -> CloudBackfillCursor {
    CloudBackfillCursor {
        last_start_time: project.resume_start_time,
        last_row_id: project.resume_row_id,
        last_span_id: project.resume_span_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_row(status: BulkJobProjectStatus) -> BulkJobProject {
        let now = chrono::Utc::now();
        BulkJobProject {
            id: 1,
            job_id: Uuid::nil(),
            project_id: 7,
            status,
            skip_reason: None,
            window_from: now - chrono::Duration::days(7),
            window_to: now,
            estimated_spans: 0,
            estimated_bytes: 0,
            spans_shipped: 0,
            bytes_shipped: 0,
            resume_start_time: None,
            resume_row_id: None,
            resume_span_id: None,
            last_error: None,
            started_at: None,
            completed_at: None,
        }
    }

    fn job_row(status: BulkJobStatus) -> BulkJob {
        BulkJob {
            id: Uuid::nil(),
            trigger: BulkJobTrigger::Operator,
            requested_by_user_id: Some(1),
            status,
            estimated_spans: 0,
            estimated_bytes: 0,
            spans_shipped: 0,
            bytes_shipped: 0,
            plan_hash: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            cancel_requested_at: None,
            abort_reason: None,
        }
    }

    #[test]
    fn a_fresh_project_row_resumes_from_the_start_of_its_window() {
        // The default cursor is what `backfill_cloud_telemetry_window` reads as
        // "no cursor yet". If this ever produced something else, a brand-new
        // project would silently skip the head of its own window.
        assert_eq!(
            cursor_of(&project_row(BulkJobProjectStatus::Pending)),
            CloudBackfillCursor::default()
        );
    }

    #[test]
    fn a_persisted_cursor_round_trips_through_the_row_unchanged() {
        // This is the whole of the resume guarantee: what the backfill handed
        // back after the last acknowledged chunk is exactly what it gets on the
        // next start. A dropped tiebreaker here re-ships or skips a whole
        // millisecond's worth of spans.
        let mut row = project_row(BulkJobProjectStatus::Backfilling);
        let at =
            chrono::DateTime::from_timestamp_millis(1_700_000_123_456).expect("valid timestamp");
        row.resume_start_time = Some(at);
        row.resume_row_id = Some(4_242);
        row.resume_span_id = Some("00f067aa0ba902b7".to_string());

        assert_eq!(
            cursor_of(&row),
            CloudBackfillCursor {
                last_start_time: Some(at),
                last_row_id: Some(4_242),
                last_span_id: Some("00f067aa0ba902b7".to_string()),
            }
        );
    }

    #[test]
    fn a_job_is_finished_only_when_every_project_is_terminal() {
        let detail = |statuses: &[BulkJobProjectStatus]| BulkJobDetail {
            job: job_row(BulkJobStatus::Running),
            projects: statuses.iter().copied().map(project_row).collect(),
        };

        assert!(detail(&[
            BulkJobProjectStatus::Done,
            BulkJobProjectStatus::Skipped,
            BulkJobProjectStatus::Failed
        ])
        .is_finished());
        assert_eq!(
            detail(&[
                BulkJobProjectStatus::Done,
                BulkJobProjectStatus::Backfilling
            ])
            .pending_projects(),
            1,
            "a project still shipping must keep the job open"
        );
        assert!(!detail(&[BulkJobProjectStatus::Switching]).is_finished());
    }

    #[test]
    fn every_skip_reason_is_machine_readable_and_says_where_to_fix_it_when_it_can() {
        // A skip that cannot be acted on is indistinguishable from a bug.
        assert_eq!(
            BulkSkipReason::FidelityNotQueryable.as_str(),
            "fidelity_not_queryable"
        );
        assert_eq!(
            BulkSkipReason::FidelityNotQueryable
                .setup_path(9)
                .as_deref(),
            Some("/projects/9/settings/telemetry")
        );
        // A deleted project has nowhere to send the operator, and inventing a
        // link to a page that 404s would be worse than none.
        assert_eq!(BulkSkipReason::ProjectNotFound.setup_path(9), None);
    }

    #[test]
    fn every_abort_reason_names_the_fix_and_the_page_that_applies_it() {
        for reason in [
            BulkAbortReason::NotLinked,
            BulkAbortReason::CredentialRejected,
            BulkAbortReason::TelemetryExportDisabled,
        ] {
            let detail = reason.detail();
            assert!(
                detail.contains(CLOUD_SETUP_PATH),
                "{reason} must link to the Cloud setup page: {detail}"
            );
            assert!(
                detail.contains("resume"),
                "{reason} must tell the operator the job can be resumed: {detail}"
            );
        }
        // The scope conflict is the one that is not fixed on a settings page —
        // it is fixed by whatever else is shipping finishing.
        let busy = BulkAbortReason::SubmissionScopeBusy.detail();
        assert!(busy.contains("backfill cloud-telemetry"), "{busy}");
    }

    #[test]
    fn error_messages_carry_the_ids_needed_to_act_on_them() {
        let job_id = Uuid::new_v4();
        let already = CloudBulkActivationError::JobAlreadyActive {
            job_id,
            status: BulkJobStatus::Running,
        };
        let message = already.to_string();
        assert!(message.contains(&job_id.to_string()), "{message}");
        assert!(message.contains("running"), "{message}");

        let missing = CloudBulkActivationError::JobProjectNotFound {
            job_id,
            project_id: 12,
        };
        assert!(missing.to_string().contains("project 12"));

        let window = CloudBulkActivationError::InvalidWindow {
            project_id: 12,
            from: "2026-09-02T00:00:00Z".into(),
            to: "2026-09-01T00:00:00Z".into(),
        };
        let message = window.to_string();
        assert!(message.contains("Project 12"), "{message}");
        assert!(message.contains("2026-09-02T00:00:00Z"), "{message}");
    }

    #[test]
    fn span_counts_saturate_rather_than_wrapping_negative() {
        assert_eq!(clamp_to_i64(0), 0);
        assert_eq!(clamp_to_i64(1_000), 1_000);
        assert_eq!(clamp_to_i64(u64::MAX), i64::MAX);
    }
}
