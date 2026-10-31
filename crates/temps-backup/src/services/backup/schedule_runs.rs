// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Schedule runs, enqueueing fan-out runs, pending backup rows, listing and
//! cancelling runs.

use super::*;

fn schedule_run_aggregate_state(
    total_jobs: i64,
    failed_jobs: i64,
    running_jobs: i64,
    pending_jobs: i64,
) -> &'static str {
    if total_jobs == 0 {
        "skipped"
    } else if pending_jobs + running_jobs > 0 {
        "running"
    } else if failed_jobs > 0 {
        "failed"
    } else {
        "completed"
    }
}

impl BackupService {
    /// Paginated run history for a backup schedule (deliverable 1).
    /// List run history for a backup schedule, one row per scheduler tick.
    ///
    /// Returns one [`ScheduleRunSummary`] per `schedule_runs` row linked to
    /// the schedule, with child backup counts aggregated in a single SQL
    /// round-trip. The `aggregate_state` is computed in Rust from the counts:
    ///
    /// - `"running"` — `pending_jobs + running_jobs > 0`
    /// - `"failed"` — `failed_jobs > 0` and `running_jobs + pending_jobs == 0`
    /// - `"completed"` — all children completed
    ///
    /// Legacy `backups` rows that have `schedule_id` set but no
    /// `schedule_run_id` (pre-fan-out history) are surfaced as synthetic
    /// single-job runs in the same list so old history does not disappear.
    ///
    /// `page` is 1-based and clamped to `1` when `< 1`.
    /// `page_size` is clamped to `100` when `> 100` and defaults to `20`.
    pub async fn list_schedule_runs(
        &self,
        schedule_id: i32,
        page: i64,
        page_size: i64,
    ) -> Result<ScheduleRunSummaryList, BackupError> {
        // Verify the schedule exists first so we return 404 instead of an
        // empty page when the caller passes an unknown id.
        self.get_backup_schedule(schedule_id).await?;

        // Clamp pagination parameters.
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let offset = (page - 1) * page_size;

        // ── Count total run rows (real + synthetic legacy) ────────────────────

        #[derive(FromQueryResult)]
        struct CountRow {
            total: i64,
        }

        // Real runs: schedule_runs rows.
        // Legacy rows: backups with schedule_id set but schedule_run_id NULL.
        let count_sql = r#"
SELECT (
    SELECT COUNT(*) FROM schedule_runs WHERE schedule_id = $1
) + (
    SELECT COUNT(*) FROM backups
     WHERE schedule_id = $1
       AND schedule_run_id IS NULL
) AS total
        "#;

        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            count_sql,
            vec![Value::from(schedule_id)],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?
        .map(|r| r.total)
        .unwrap_or(0);

        // ── Fetch page — one row per tick + synthetic legacy rows ─────────────

        #[derive(FromQueryResult)]
        struct RunRow {
            run_id: i64,
            schedule_id: i32,
            triggered_by: String,
            started_at: chrono::DateTime<chrono::Utc>,
            finished_at: Option<chrono::DateTime<chrono::Utc>>,
            total_jobs: i64,
            completed_jobs: i64,
            failed_jobs: i64,
            running_jobs: i64,
            pending_jobs: i64,
        }

        // Real rows: one per schedule_runs row, child counts via LEFT JOIN.
        // Legacy rows (backups with schedule_id set, schedule_run_id NULL):
        // synthesised as if they were a run with a single job whose state
        // is the backup's own state. We use the backups.id negated as the
        // synthetic run_id to avoid collisions with real schedule_runs.id
        // (both are distinct integer ranges; negative is never a real run id).
        let sql = r#"
SELECT
    sr.id                AS run_id,
    sr.schedule_id       AS schedule_id,
    sr.triggered_by      AS triggered_by,
    sr.started_at        AS started_at,
    sr.finished_at       AS finished_at,
    COUNT(b.id)                                                   AS total_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'completed')              AS completed_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'failed')                 AS failed_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'running')                AS running_jobs,
    COUNT(b.id) FILTER (WHERE b.state = 'pending')                AS pending_jobs
FROM schedule_runs sr
LEFT JOIN backups b ON b.schedule_run_id = sr.id
WHERE sr.schedule_id = $1
GROUP BY sr.id

UNION ALL

-- Synthetic rows for legacy backups (pre-fan-out, no schedule_run_id).
SELECT
    (-b.id)::BIGINT      AS run_id,
    b.schedule_id        AS schedule_id,
    'cron'               AS triggered_by,
    b.started_at         AS started_at,
    b.finished_at        AS finished_at,
    1                    AS total_jobs,
    CASE WHEN b.state = 'completed' THEN 1 ELSE 0 END AS completed_jobs,
    CASE WHEN b.state = 'failed'    THEN 1 ELSE 0 END AS failed_jobs,
    CASE WHEN b.state = 'running'   THEN 1 ELSE 0 END AS running_jobs,
    CASE WHEN b.state = 'pending'   THEN 1 ELSE 0 END AS pending_jobs
FROM backups b
WHERE b.schedule_id = $1
  AND b.schedule_run_id IS NULL

ORDER BY started_at DESC
LIMIT  $2
OFFSET $3
        "#;

        let raw_rows = RunRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                Value::from(schedule_id),
                Value::from(page_size),
                Value::from(offset),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        // ── Compute aggregate_state in Rust ───────────────────────────────────

        let runs = raw_rows
            .into_iter()
            .map(|r| {
                let aggregate_state = schedule_run_aggregate_state(
                    r.total_jobs,
                    r.failed_jobs,
                    r.running_jobs,
                    r.pending_jobs,
                )
                .to_string();

                ScheduleRunSummary {
                    run_id: r.run_id,
                    schedule_id: r.schedule_id,
                    triggered_by: r.triggered_by,
                    started_at: r.started_at.to_rfc3339(),
                    finished_at: r.finished_at.map(|t| t.to_rfc3339()),
                    aggregate_state,
                    total_jobs: r.total_jobs,
                    completed_jobs: r.completed_jobs,
                    failed_jobs: r.failed_jobs,
                    running_jobs: r.running_jobs,
                    pending_jobs: r.pending_jobs,
                }
            })
            .collect();

        Ok(ScheduleRunSummaryList {
            runs,
            total,
            page,
            page_size,
        })
    }

    /// List the individual backup jobs belonging to a single scheduler run.
    ///
    /// Used by the schedule detail page's expandable accordion for per-run
    /// drill-down. Returns each child `backups` row joined with its
    /// `external_services` row (for the service name) and the most recent
    /// `backup_jobs` row (for the engine key).
    ///
    /// `page_size` defaults to 50 and is capped at 200 — a single scheduler
    /// tick produces at most 1 + N external services rows (small N).
    pub async fn list_schedule_run_jobs(
        &self,
        run_id: i64,
        page: i64,
        page_size: i64,
    ) -> Result<(Vec<ScheduleRunJobEntry>, i64), BackupError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 200);
        let offset = (page - 1) * page_size;

        #[derive(FromQueryResult)]
        struct CountRow {
            total: i64,
        }

        let count_sql = r#"
SELECT COUNT(*) AS total FROM backups WHERE schedule_run_id = $1
        "#;

        let total = CountRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            count_sql,
            vec![Value::from(run_id)],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?
        .map(|r| r.total)
        .unwrap_or(0);

        // The engine key is stored on `backups.metadata` JSON (written at
        // trigger time by `BackupService::create_pending_*`). Read it from
        // there. external_service_backups joins to external_services for
        // the human-readable name.
        let sql = r#"
SELECT
    b.id                                            AS backup_id,
    b.backup_id                                     AS backup_uuid,
    COALESCE(b.metadata::jsonb ->> 'engine', 'control_plane') AS engine,
    COALESCE(es.name, esb.service_name_snapshot, 'control plane') AS service_name,
    esb.service_id                                  AS service_id,
    b.state                                         AS state,
    b.started_at                                    AS started_at,
    b.finished_at                                   AS finished_at,
    b.size_bytes                                    AS size_bytes,
    b.error_message                                 AS error_message,
    b.s3_source_id                                  AS s3_source_id
FROM backups b
LEFT JOIN external_service_backups esb ON esb.backup_id = b.id
LEFT JOIN external_services es ON es.id = esb.service_id
WHERE b.schedule_run_id = $1
ORDER BY b.id
LIMIT  $2
OFFSET $3
        "#;

        let rows = ScheduleRunJobEntry::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            vec![
                Value::from(run_id),
                Value::from(page_size),
                Value::from(offset),
            ],
        ))
        .all(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?;

        Ok((rows, total))
    }

    /// Immediately enqueue a fan-out run for the given schedule (Run Now).
    ///
    /// Delegates to [`enqueue_scheduled_run`] with `TriggerSource::Manual`.
    /// Returns `409 Conflict` (via [`BackupError::ScheduleRunAlreadyInFlight`])
    /// when a run for this schedule already has `finished_at IS NULL`.
    ///
    /// Returns [`BackupError::Validation`] when the schedule is disabled.
    /// Cancel a single backup by `backups.id`. Flips the row + its latest
    /// `backup_jobs` row to `failed` with a "cancelled by user" reason.
    /// Returns the number of rows updated — `0` means the backup was already
    /// terminal (which the caller should treat as an idempotent success,
    /// not a 404). The runner's in-process cancellation token is observed
    /// on the next heartbeat tick (≤5s) so the engine exits cleanly and
    /// `rollback` reaps the sidecar.
    pub async fn cancel_backup(
        &self,
        backup_id: i32,
        triggered_by_user_id: Option<i32>,
    ) -> Result<u64, BackupError> {
        // Verify the backup exists so the caller gets a real 404 (not an
        // "everything looks fine, nothing happened" silent no-op) when the
        // id is wrong. Then delegate to `temps_backup_core::cancel_backup`
        // which owns the actual DB writes.
        temps_entities::backups::Entity::find_by_id(backup_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "Backup".to_string(),
                detail: format!("Backup {} not found", backup_id),
            })?;

        let reason = match triggered_by_user_id {
            Some(uid) => format!("cancelled by user {}", uid),
            None => "cancelled".to_string(),
        };

        let rows = temps_backup_core::cancel_backup(self.db.as_ref(), backup_id, &reason)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!("Failed to cancel backup {}: {}", backup_id, e),
            })?;

        // Notify the processor so any in-flight engine task fires its
        // cancel token and exits cleanly. The DB flip above already
        // marked the row as failed; this signal stops the running
        // container so the sidecar gets reaped promptly.
        if let Err(e) = self
            .queue
            .send(temps_core::Job::BackupCancelRequested(
                temps_core::BackupCancelRequestedJob { backup_id },
            ))
            .await
        {
            warn!(
                backup_id,
                error = %e,
                "cancel_backup: queue.send for BackupCancelRequested failed; running engine (if any) will not be interrupted promptly",
            );
        }

        info!(
            backup_id,
            rows_affected = rows,
            triggered_by_user_id = ?triggered_by_user_id,
            "BackupService: cancel_backup completed",
        );

        Ok(rows)
    }

    /// Cancel every non-terminal child backup belonging to a scheduler run.
    /// Returns the number of children that were flipped to `failed`. The
    /// parent `schedule_runs.finished_at` is stamped automatically once no
    /// live children remain (which is true after a successful cancel).
    pub async fn cancel_schedule_run(
        &self,
        schedule_run_id: i64,
        triggered_by_user_id: Option<i32>,
    ) -> Result<u64, BackupError> {
        // Verify the run exists so the caller gets a real 404.
        temps_entities::schedule_runs::Entity::find_by_id(schedule_run_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ScheduleRun".to_string(),
                detail: format!("Schedule run {} not found", schedule_run_id),
            })?;

        let reason = match triggered_by_user_id {
            Some(uid) => format!(
                "cancelled by user {} (run {} cancelled)",
                uid, schedule_run_id
            ),
            None => format!("cancelled (run {} cancelled)", schedule_run_id),
        };

        // Capture live child ids BEFORE the DB helper flips them — we
        // need them to signal the in-process consumer for each one.
        #[derive(FromQueryResult)]
        struct ChildId {
            id: i32,
        }
        let live_children = ChildId::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT id FROM backups
                WHERE schedule_run_id = $1 AND state IN ('pending', 'running')"#,
            vec![Value::from(schedule_run_id)],
        ))
        .all(self.db.as_ref())
        .await?;

        let cancelled =
            temps_backup_core::cancel_schedule_run(self.db.as_ref(), schedule_run_id, &reason)
                .await
                .map_err(|e| BackupError::Internal {
                    message: format!("Failed to cancel schedule run {}: {}", schedule_run_id, e),
                })?;

        // Publish a cancel signal per live child so any in-flight engine
        // tasks fire their cancel tokens and exit cleanly.
        let queue = &self.queue;
        for child in live_children {
            if let Err(e) = queue
                .send(temps_core::Job::BackupCancelRequested(
                    temps_core::BackupCancelRequestedJob {
                        backup_id: child.id,
                    },
                ))
                .await
            {
                warn!(
                    schedule_run_id,
                    backup_id = child.id,
                    error = %e,
                    "cancel_schedule_run: queue.send for child cancel failed",
                );
            }
        }

        info!(
            schedule_run_id,
            cancelled,
            triggered_by_user_id = ?triggered_by_user_id,
            "BackupService: cancel_schedule_run completed",
        );

        Ok(cancelled)
    }

    pub async fn run_schedule_now(
        &self,
        schedule_id: i32,
        triggered_by_user_id: Option<i32>,
    ) -> Result<ScheduleRunResponse, BackupError> {
        let schedule = self.get_backup_schedule(schedule_id).await?;

        if !schedule.enabled {
            return Err(BackupError::Validation(format!(
                "Schedule {} is disabled; enable it before triggering a manual run",
                schedule_id
            )));
        }

        match self
            .enqueue_scheduled_run(&schedule, TriggerSource::Manual, triggered_by_user_id)
            .await?
        {
            ScheduleRunOutcome::Started { run_id, jobs } => {
                info!(
                    schedule_id = schedule.id,
                    schedule_name = %schedule.name,
                    run_id,
                    job_count = jobs.len(),
                    "run_schedule_now: fan-out run started",
                );
                Ok(ScheduleRunResponse {
                    schedule_run_id: run_id,
                    jobs,
                })
            }
            ScheduleRunOutcome::AlreadyInFlight { existing_run_id } => {
                Err(BackupError::ScheduleRunAlreadyInFlight { existing_run_id })
            }
        }
    }

    /// Fan-out a scheduler tick into one `schedule_runs` row + one control-plane
    /// backup job + one backup job per supported external service.
    ///
    /// ## Fan-out logic (in one transaction)
    ///
    /// 1. Check for an existing in-flight run (any `schedule_runs` row for this
    ///    schedule with `finished_at IS NULL`). If found, return
    ///    [`ScheduleRunOutcome::AlreadyInFlight`] immediately.
    /// 2. Insert a `schedule_runs` row. Capture `run_id`.
    /// 3. Insert a control-plane `backups` row with `schedule_run_id = run_id`.
    ///    Enqueue a `backup_jobs` row for `engine = "control_plane"`.
    /// 4. Load every `external_services` row and attempt to resolve its engine
    ///    key via [`resolve_engine_key`]. Skip rows where resolution returns
    ///    `Err` (unsupported type) — log at `warn` with the service id and type.
    /// 5. For each supported service, insert an `external_service_backups` row +
    ///    parent `backups` row (`schedule_run_id = run_id`), then enqueue a
    ///    `backup_jobs` row. Individual `AlreadyInFlight` responses from the
    ///    concurrency guard are logged at `info` and skipped — the rest of the
    ///    fan-out continues.
    /// 6. Advance `backup_schedules.next_run`, `last_run`, `last_job_id`.
    /// 7. Commit and return [`ScheduleRunOutcome::Started`].
    ///
    /// The cron caller treats both outcome variants as `Ok` and logs the run id.
    /// The "Run now" handler converts `AlreadyInFlight` to a `409 Conflict`.
    /// Close any `schedule_runs` rows for this schedule that have
    /// `finished_at IS NULL` but no longer have any pending/running children.
    ///
    /// Earlier code revisions could leave the parent row open if a worker
    /// crashed between writing the last child's terminal state and calling
    /// `mark_schedule_run_finished_if_done`. Without this reconciler the
    /// concurrency guard would refuse all future "Run now" requests for the
    /// schedule. The UPDATE is idempotent and a no-op for healthy schedules.
    ///
    /// Sets `finished_at` to the latest child `finished_at` so duration
    /// metrics remain accurate; falls back to `NOW()` if every child is
    /// missing a `finished_at` (shouldn't happen, but safer than NULL).
    async fn reconcile_drifted_schedule_runs(&self, schedule_id: i32) -> Result<(), BackupError> {
        use sea_orm::ConnectionTrait;

        let sql = r#"
UPDATE schedule_runs sr
   SET finished_at = COALESCE(
       (SELECT MAX(b.finished_at)
          FROM backups b
         WHERE b.schedule_run_id = sr.id),
       NOW()
   )
 WHERE sr.schedule_id = $1
   AND sr.finished_at IS NULL
   AND NOT EXISTS (
       SELECT 1 FROM backups b
        WHERE b.schedule_run_id = sr.id
          AND b.state IN ('pending', 'running')
   )
   AND EXISTS (
       SELECT 1 FROM backups b
        WHERE b.schedule_run_id = sr.id
   )
        "#;

        self.db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![Value::from(schedule_id)],
            ))
            .await
            .map_err(BackupError::Database)?;

        Ok(())
    }

    pub async fn enqueue_scheduled_run(
        &self,
        schedule: &temps_entities::backup_schedules::Model,
        triggered_by: TriggerSource,
        triggered_by_user_id: Option<i32>,
    ) -> Result<ScheduleRunOutcome, BackupError> {
        use sea_orm::{Set, TransactionTrait};

        let now = chrono::Utc::now();

        // Compute next_run before opening the transaction so a parse error
        // fails fast without wasted DB work.
        let cron_schedule = Schedule::from_str(&schedule.schedule_expression).map_err(|e| {
            BackupError::Validation(format!(
                "Invalid cron expression for schedule {}: {}",
                schedule.id, e
            ))
        })?;
        let next_run = cron_schedule.upcoming(Utc).next();

        // ── Step 1: in-flight check (before opening the write transaction) ────
        //
        // A run is only "in flight" if at least one child backup is still
        // `pending` or `running`. We do NOT rely on `schedule_runs.finished_at`
        // alone — that field can drift to NULL if a prior worker crashed
        // between writing the last child's terminal state and calling
        // `mark_schedule_run_finished_if_done`, leaving the schedule
        // permanently un-runnable. Checking children directly is also the
        // authoritative source of truth used by the aggregate-state SQL in
        // `list_schedule_runs`, so the guard and the UI agree by construction.
        //
        // While we're here, opportunistically close any drifted `schedule_runs`
        // rows so the guard, the UI, and `finished_at`-based queries stay in
        // sync after we let this new run through.
        self.reconcile_drifted_schedule_runs(schedule.id).await?;

        #[derive(FromQueryResult)]
        struct InFlightRow {
            id: i64,
        }

        let in_flight_sql = r#"
SELECT sr.id FROM schedule_runs sr
 WHERE sr.schedule_id = $1
   AND EXISTS (
       SELECT 1 FROM backups b
        WHERE b.schedule_run_id = sr.id
          AND b.state IN ('pending', 'running')
   )
 LIMIT 1
        "#;

        if let Some(existing) = InFlightRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            in_flight_sql,
            vec![Value::from(schedule.id)],
        ))
        .one(self.db.as_ref())
        .await
        .map_err(BackupError::Database)?
        {
            return Ok(ScheduleRunOutcome::AlreadyInFlight {
                existing_run_id: existing.id,
            });
        }

        // ── Step 2: connect to Docker for engine resolution ───────────────────
        // We connect once per tick, not per service, to amortise the socket
        // setup cost. `connect_with_local_defaults()` is fast (no round-trip).
        let docker =
            bollard::Docker::connect_with_local_defaults().map_err(|e| BackupError::Internal {
                message: format!(
                    "enqueue_scheduled_run: failed to connect to Docker for engine resolution \
                     (schedule {}): {}",
                    schedule.id, e
                ),
            })?;

        // Load the external services this schedule should fan out to.
        // Two modes (set on `backup_schedules.target_all_services`):
        //   - true  → every external service on the host (auto-includes
        //             future databases); the explicit join table is ignored.
        //   - false → only services attached via `backup_schedule_services`
        //             (the operator picked specific DBs).
        use sea_orm::{ColumnTrait, QueryFilter};
        let backing_service_ids = self
            .source_backing_service_ids(schedule.s3_source_id)
            .await?;
        let external_services = if schedule.target_all_services {
            temps_entities::external_services::Entity::find()
                .all(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?
        } else {
            temps_entities::external_services::Entity::find()
                .inner_join(temps_entities::backup_schedule_services::Entity)
                .filter(
                    temps_entities::backup_schedule_services::Column::ScheduleId.eq(schedule.id),
                )
                .all(self.db.as_ref())
                .await
                .map_err(BackupError::Database)?
        };
        let external_services =
            exclude_source_backing_services(external_services, &backing_service_ids);

        if external_services.is_empty() {
            // Two reasons we could end up here: no DBs exist yet, or the
            // operator picked "specific" mode and didn't attach anything.
            // Log both with `target_all_services` so it's obvious which.
            info!(
                schedule_id = schedule.id,
                schedule_name = %schedule.name,
                target_all_services = schedule.target_all_services,
                "enqueue_scheduled_run: no external services in scope; fan-out will be control-plane only",
            );
        }

        // Resolve engine keys outside the transaction (async Docker probes).
        let mut resolved_services: Vec<(temps_entities::external_services::Model, &'static str)> =
            Vec::with_capacity(external_services.len());

        for svc in external_services {
            match crate::engines::dispatch::resolve_engine_key(&svc, &docker).await {
                Ok(engine_key) => {
                    resolved_services.push((svc, engine_key));
                }
                Err(e) => {
                    warn!(
                        service_id = svc.id,
                        service_type = %svc.service_type,
                        error = %e,
                        "enqueue_scheduled_run: skipping unsupported external service",
                    );
                }
            }
        }
        let expected_service_ids = resolved_services
            .iter()
            .map(|(service, _)| service.id)
            .collect::<Vec<_>>();

        if !schedule.include_control_plane && resolved_services.is_empty() {
            return Err(BackupError::Validation(format!(
                "Schedule {} has no eligible backup targets; no run was recorded",
                schedule.id
            )));
        }

        // ── Step 3: open the write transaction ────────────────────────────────

        let txn = self.db.begin().await?;

        // Insert the schedule_runs row.
        let run_insert_sql = r#"
INSERT INTO schedule_runs (schedule_id, triggered_by, triggered_by_user_id, started_at, created_at)
VALUES ($1, $2, $3, NOW(), NOW())
RETURNING id
        "#;

        #[derive(FromQueryResult)]
        struct RunIdRow {
            id: i64,
        }

        let run_id = RunIdRow::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            run_insert_sql,
            vec![
                Value::from(schedule.id),
                Value::from(triggered_by.as_str().to_owned()),
                Value::from(triggered_by_user_id),
            ],
        ))
        .one(&txn)
        .await
        .map_err(BackupError::Database)?
        .ok_or_else(|| BackupError::Internal {
            message: format!(
                "enqueue_scheduled_run: INSERT into schedule_runs returned no id \
                 for schedule {}",
                schedule.id
            ),
        })?
        .id;

        let mut jobs: Vec<EnqueuedJob> = Vec::new();

        // Defer queue publishes until after txn.commit so the consumer
        // can't dispatch an engine against a backups row the txn might
        // still roll back.
        let mut deferred_messages: Vec<temps_core::BackupRequestedJob> = Vec::new();

        // ── Step 4: control-plane backup (skipped when the schedule
        // ──         opts out of control-plane coverage). ─────────────────────
        if schedule.include_control_plane {
            let cp_uuid = Uuid::new_v4().to_string();
            let cp_backup = temps_entities::backups::ActiveModel {
                id: sea_orm::NotSet,
                name: Set(format!("Backup {}", cp_uuid)),
                backup_id: Set(cp_uuid.clone()),
                schedule_id: Set(Some(schedule.id)),
                schedule_run_id: Set(Some(run_id)),
                backup_type: Set(schedule.backup_type.clone()),
                state: Set("pending".to_string()),
                started_at: Set(now),
                finished_at: Set(None),
                s3_source_id: Set(schedule.s3_source_id),
                s3_location: Set(String::new()),
                compression_type: Set("gzip".to_string()),
                created_by: Set(0),
                tags: Set("[]".to_string()),
                size_bytes: Set(None),
                file_count: Set(None),
                error_message: Set(None),
                expires_at: Set(retention_expiry(now, schedule.retention_period)),
                checksum: Set(None),
                metadata: Set(serde_json::json!({
                    "engine": "control_plane",
                    "async_runner": true,
                    "scheduled": triggered_by == TriggerSource::Cron,
                    "schedule_id": schedule.id,
                    "run_id": run_id,
                    "expected_service_ids": expected_service_ids,
                    "timestamp": now.to_rfc3339(),
                })
                .to_string()),
            };

            let cp_backup_row = cp_backup.insert(&txn).await?;

            deferred_messages.push(temps_core::BackupRequestedJob {
                backup_id: cp_backup_row.id,
                engine: "control_plane".to_string(),
                params: serde_json::json!({
                    "s3_source_id": schedule.s3_source_id,
                    "schedule_id": schedule.id,
                    "run_id": run_id,
                }),
                max_runtime_secs: schedule.max_runtime_secs.unwrap_or(4 * 60 * 60),
            });
            jobs.push(EnqueuedJob {
                backup_id: cp_backup_row.id,
                job_id: cp_backup_row.id as i64,
                engine: "control_plane".to_string(),
                target_service_id: None,
            });
        } else {
            info!(
                schedule_id = schedule.id,
                run_id,
                "enqueue_scheduled_run: include_control_plane=false, skipping control-plane backup",
            );
        }

        // ── Step 5: external service backups ──────────────────────────────────

        for (svc, engine_key) in &resolved_services {
            let trigger = BackupTriggerParams {
                engine: engine_key.to_string(),
                params: serde_json::json!({
                    "service_id": svc.id,
                    "s3_source_id": schedule.s3_source_id,
                    "backup_type": schedule.backup_type,
                    "schedule_id": schedule.id,
                    "run_id": run_id,
                }),
                max_runtime_secs: schedule.max_runtime_secs,
            };

            match self
                .insert_pending_external_service_backup_in_txn(
                    &txn,
                    svc.id,
                    schedule.s3_source_id,
                    &schedule.backup_type,
                    0,
                    "gzip",
                    Some(ScheduleRunContext {
                        schedule_id: schedule.id,
                        schedule_run_id: run_id,
                        retention_period: schedule.retention_period,
                    }),
                    &trigger,
                )
                .await
            {
                Ok((parent_row, _esb_row)) => {
                    deferred_messages.push(temps_core::BackupRequestedJob {
                        backup_id: parent_row.id,
                        engine: trigger.engine.clone(),
                        params: trigger.params.clone(),
                        max_runtime_secs: trigger.max_runtime_secs.unwrap_or(4 * 60 * 60),
                    });
                    jobs.push(EnqueuedJob {
                        backup_id: parent_row.id,
                        job_id: parent_row.id as i64,
                        engine: engine_key.to_string(),
                        target_service_id: Some(svc.id),
                    });
                }
                Err(e) => {
                    error!(
                        schedule_id = schedule.id,
                        service_id = svc.id,
                        service_name = %svc.name,
                        engine = engine_key,
                        error = %e,
                        "enqueue_scheduled_run: failed to insert external service rows; rolling back the fan-out",
                    );
                    return Err(e);
                }
            }
        }

        // ── Step 6: advance schedule metadata ─────────────────────────────────
        let mut schedule_update: temps_entities::backup_schedules::ActiveModel =
            schedule.clone().into_active_model();
        schedule_update.next_run = Set(next_run);
        schedule_update.last_run = Set(Some(now));
        schedule_update.update(&txn).await?;

        // ── Commit ────────────────────────────────────────────────────────────

        txn.commit().await?;

        // ── Publish queue messages after commit ───────────────────────────────
        // Publishing after commit guarantees the consumer never dispatches
        // an engine against a backups row that the txn might roll back.
        let queue = &self.queue;
        for req in deferred_messages {
            let backup_id = req.backup_id;
            if let Err(e) = queue.send(temps_core::Job::BackupRequested(req)).await {
                warn!(
                    schedule_id = schedule.id,
                    backup_id,
                    error = %e,
                    "enqueue_scheduled_run: queue.send failed (row committed but not dispatched)",
                );
            }
        }

        info!(
            schedule_id = schedule.id,
            schedule_name = %schedule.name,
            run_id,
            job_count = jobs.len(),
            triggered_by = triggered_by.as_str(),
            "enqueue_scheduled_run: fan-out committed and dispatched",
        );

        Ok(ScheduleRunOutcome::Started { run_id, jobs })
    }

    /// Insert a pending control-plane `backups` row and dispatch the
    /// matching engine task on the executor. The synthetic `i64` returned
    /// is the backup row's id widened for backwards-compat with callers
    /// that previously took a `backup_jobs.id`.
    pub async fn create_pending_backup_row(
        &self,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
        trigger: BackupTriggerParams,
    ) -> Result<(Backup, i64), BackupError> {
        use sea_orm::Set;

        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!("S3 source {} not found", s3_source_id),
            })?;

        let backup_uuid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let new_backup = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(format!("Backup {}", backup_uuid)),
            backup_id: Set(backup_uuid.clone()),
            schedule_id: Set(None),
            schedule_run_id: sea_orm::NotSet,
            backup_type: Set(backup_type.to_string()),
            state: Set("pending".to_string()),
            started_at: Set(now),
            finished_at: Set(None),
            s3_source_id: Set(s3_source_id),
            s3_location: Set(String::new()),
            compression_type: Set("gzip".to_string()),
            created_by: Set(created_by),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(None),
            checksum: Set(None),
            metadata: Set(serde_json::json!({
                "engine": trigger.engine,
                "async_runner": true,
                "timestamp": now.to_rfc3339(),
            })
            .to_string()),
        };

        let backup = new_backup.insert(self.db.as_ref()).await?;
        let backup_id = backup.id;

        // Publish to the queue. If publish fails the row is left in
        // `pending`; the next boot's reconcile will flip it to `failed`.
        let max_runtime_secs = trigger.max_runtime_secs.unwrap_or(4 * 60 * 60);
        if let Err(e) = self
            .queue
            .send(temps_core::Job::BackupRequested(
                temps_core::BackupRequestedJob {
                    backup_id,
                    engine: trigger.engine.clone(),
                    params: trigger.params,
                    max_runtime_secs,
                },
            ))
            .await
        {
            return Err(BackupError::Internal {
                message: format!(
                    "Failed to publish BackupRequested for backup {}: {}",
                    backup_id, e
                ),
            });
        }

        info!(
            backup_id = %backup.backup_id,
            s3_source_id,
            backup_row_id = backup_id,
            "BackupService: created pending backup row and published BackupRequested",
        );

        Ok((backup, backup_id as i64))
    }

    /// Insert parent `backups` + child `external_service_backups` rows inside
    /// the supplied transaction. Does NOT spawn the engine task — the caller
    /// must call `executor.spawn` AFTER committing the transaction so the
    /// engine never sees a row the txn might roll back.
    ///
    /// Used by `enqueue_scheduled_run`'s fan-out. Manual triggers go through
    /// [`create_pending_external_service_backup_row`] instead, which handles
    /// the txn lifecycle internally.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_pending_external_service_backup_in_txn(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        service_id: i32,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
        compression_type: &str,
        schedule_ctx: Option<ScheduleRunContext>,
        trigger: &BackupTriggerParams,
    ) -> Result<
        (
            temps_entities::backups::Model,
            temps_entities::external_service_backups::Model,
        ),
        BackupError,
    > {
        use sea_orm::Set;

        let backup_uuid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let source_service = temps_entities::external_services::Entity::find_by_id(service_id)
            .one(txn)
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!(
                    "service {} disappeared while creating its backup",
                    service_id
                ),
            })?;

        let mut backups_metadata = serde_json::Map::new();
        backups_metadata.insert(
            "engine".to_string(),
            serde_json::Value::String(trigger.engine.clone()),
        );
        backups_metadata.insert("async_runner".to_string(), serde_json::Value::Bool(true));
        backups_metadata.insert(
            "external_service_id".to_string(),
            serde_json::Value::Number(service_id.into()),
        );
        backups_metadata.insert(
            "service_id".to_string(),
            serde_json::Value::Number(service_id.into()),
        );
        backups_metadata.insert(
            "timestamp".to_string(),
            serde_json::Value::String(now.to_rfc3339()),
        );
        if let Some(ctx) = schedule_ctx {
            backups_metadata.insert(
                "schedule_id".to_string(),
                serde_json::Value::Number(ctx.schedule_id.into()),
            );
            backups_metadata.insert(
                "run_id".to_string(),
                serde_json::Value::Number(ctx.schedule_run_id.into()),
            );
        }

        let mut esb_metadata = serde_json::Map::new();
        esb_metadata.insert(
            "engine".to_string(),
            serde_json::Value::String(trigger.engine.clone()),
        );
        esb_metadata.insert("async_runner".to_string(), serde_json::Value::Bool(true));
        esb_metadata.insert(
            "backup_uuid".to_string(),
            serde_json::Value::String(backup_uuid.clone()),
        );
        esb_metadata.insert(
            "timestamp".to_string(),
            serde_json::Value::String(now.to_rfc3339()),
        );
        if let Some(ctx) = schedule_ctx {
            esb_metadata.insert(
                "schedule_id".to_string(),
                serde_json::Value::Number(ctx.schedule_id.into()),
            );
            esb_metadata.insert(
                "run_id".to_string(),
                serde_json::Value::Number(ctx.schedule_run_id.into()),
            );
        }

        // Schedule-created backups are deleted by `run_retention_cleanup`
        // once they are older than the schedule's retention period; record
        // that deadline so the API and console can show it. Manual runs
        // (`schedule_ctx == None`) are kept until deleted.
        let expires_at = schedule_ctx.and_then(|ctx| retention_expiry(now, ctx.retention_period));

        let parent = temps_entities::backups::ActiveModel {
            id: sea_orm::NotSet,
            name: Set(format!("Backup {}", backup_uuid)),
            backup_id: Set(backup_uuid.clone()),
            schedule_id: Set(schedule_ctx.map(|c| c.schedule_id)),
            schedule_run_id: Set(schedule_ctx.map(|c| c.schedule_run_id)),
            backup_type: Set(backup_type.to_string()),
            state: Set("pending".to_string()),
            started_at: Set(now),
            finished_at: Set(None),
            s3_source_id: Set(s3_source_id),
            s3_location: Set(String::new()),
            compression_type: Set(compression_type.to_string()),
            created_by: Set(created_by),
            tags: Set("[]".to_string()),
            size_bytes: Set(None),
            file_count: Set(None),
            error_message: Set(None),
            expires_at: Set(expires_at),
            checksum: Set(None),
            metadata: Set(serde_json::Value::Object(backups_metadata).to_string()),
        }
        .insert(txn)
        .await?;

        let child = temps_entities::external_service_backups::ActiveModel {
            id: sea_orm::NotSet,
            service_id: Set(service_id),
            backup_id: Set(parent.id),
            backup_type: Set(backup_type.to_string()),
            state: Set("pending".to_string()),
            started_at: Set(now),
            finished_at: Set(None),
            size_bytes: Set(None),
            s3_location: Set(String::new()),
            error_message: Set(None),
            metadata: Set(serde_json::Value::Object(esb_metadata)),
            checksum: Set(None),
            compression_type: Set(compression_type.to_string()),
            created_by: Set(created_by),
            expires_at: Set(expires_at),
            service_name_snapshot: Set(Some(source_service.name)),
            service_type_snapshot: Set(Some(source_service.service_type)),
        }
        .insert(txn)
        .await?;

        info!(
            backup_id = %backup_uuid,
            service_id,
            s3_source_id,
            parent_row_id = parent.id,
            child_row_id = child.id,
            "BackupService: inserted pending external-service backup rows (engine task pending)",
        );

        Ok((parent, child))
    }

    /// Insert pending external-service backup rows (parent + child) and
    /// dispatch the engine task on the executor.
    ///
    /// Used by the manual `POST /external-services/{id}/run` handler. The
    /// txn is opened and committed internally; spawn happens after commit.
    ///
    /// Returns the child row and a synthetic `i64` (the parent backup row's
    /// id, widened) for backwards-compat with callers that previously used
    /// a `backup_jobs.id`.
    pub async fn create_pending_external_service_backup_row(
        &self,
        service_id: i32,
        s3_source_id: i32,
        backup_type: &str,
        created_by: i32,
        trigger: BackupTriggerParams,
    ) -> Result<(temps_entities::external_service_backups::Model, i64), BackupError> {
        temps_entities::external_services::Entity::find_by_id(service_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "ExternalService".to_string(),
                detail: format!("External service with ID {} not found", service_id),
            })?;
        temps_entities::s3_sources::Entity::find_by_id(s3_source_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| BackupError::NotFound {
                resource: "S3Source".to_string(),
                detail: format!("S3 source {} not found", s3_source_id),
            })?;

        let txn = self.db.begin().await?;
        let (parent, child) = self
            .insert_pending_external_service_backup_in_txn(
                &txn,
                service_id,
                s3_source_id,
                backup_type,
                created_by,
                "none",
                None,
                &trigger,
            )
            .await?;
        txn.commit().await?;

        let backup_id = parent.id;
        let max_runtime_secs = trigger.max_runtime_secs.unwrap_or(4 * 60 * 60);
        if let Err(e) = self
            .queue
            .send(temps_core::Job::BackupRequested(
                temps_core::BackupRequestedJob {
                    backup_id,
                    engine: trigger.engine.clone(),
                    params: trigger.params,
                    max_runtime_secs,
                },
            ))
            .await
        {
            warn!(
                backup_id,
                service_id,
                error = %e,
                "create_pending_external_service_backup_row: queue.send failed; row committed but not dispatched",
            );
        }

        Ok((child, backup_id as i64))
    }
}

#[cfg(test)]
mod tests;
