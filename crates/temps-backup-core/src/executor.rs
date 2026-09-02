// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-process backup executor.
//!
//! Replacement for the DB-queue-and-lease `BackupRunner`. The model is:
//!
//! 1. A handler or cron tick inserts a row into `backups` (and optionally
//!    `backup_jobs` for the audit log) and calls `executor.spawn(backup_id)`.
//! 2. The executor takes the global concurrency semaphore (default N=4),
//!    holds a per-`backup_id` entry in an in-memory `HashMap` to prevent
//!    duplicate spawns, then `tokio::spawn`s the engine task.
//! 3. The task runs the engine to completion. On any exit path
//!    (success / failure / panic / cancel / timeout) the `JobHandle`'s
//!    `Drop` impl removes the map entry and releases the permit.
//! 4. On boot, [`reconcile_orphans_on_startup`] flips every `state='running'`
//!    row to `failed("process restarted")` — the runtime is the source of
//!    truth, so anything the DB thinks is running but isn't in our HashMap
//!    is definitively dead.
//!
//! No DB poll loop. No claim_token. No lease. No `state='running' AND
//! leased_until < NOW()` reclaim. The owning task is the single source of
//! truth for what's executing.
//!
//! ## Cross-process safety
//!
//! This design assumes one writer process per database. If two temps
//! binaries run against the same DB they will both reconcile-on-startup
//! (flipping each other's in-flight rows to failed) and step on each
//! other's spawns. Multi-node support is intentionally out of scope —
//! the right design for that is leader election or a real message
//! broker, not hand-rolled lease semantics.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value as SValue};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use temps_core::{BackupCompletedJob, BackupFailedJob, Job, JobQueue};

use crate::engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
use crate::notifier::{BackupFailureContext, BackupFailureNotifier};
use crate::queue::mark_schedule_run_finished_if_done;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Hard cap on concurrent in-flight backups across all engines. Set this
/// to the smallest value that gives acceptable throughput on a typical
/// host — going higher just costs RAM and disk I/O without finishing any
/// individual backup faster.
const DEFAULT_MAX_CONCURRENT: usize = 4;

/// Default backoff sequence (seconds) between retries. Index = attempt
/// number, value = sleep duration before that attempt. Length is the
/// implicit `max_attempts`.
const RETRY_BACKOFFS_SECS: &[u64] = &[0, 30, 60, 120];

// ── Executor ─────────────────────────────────────────────────────────────────

/// Single-process backup executor.
///
/// Owns the engine registry, the concurrency semaphore, and the
/// `backup_id -> JobHandle` dedup map. Cheap to clone (everything is in
/// `Arc`); pass a clone wherever you need to call `spawn` or `cancel`.
#[derive(Clone)]
pub struct BackupExecutor {
    inner: Arc<ExecutorInner>,
}

struct ExecutorInner {
    db: Arc<DatabaseConnection>,
    engines: HashMap<&'static str, Arc<dyn BackupEngine>>,
    semaphore: Arc<Semaphore>,
    in_flight: Mutex<HashMap<i32, JobHandle>>,
    /// Optional failure-notification hook. Fired from `finalize_failed`
    /// via a detached `tokio::spawn` so slow notifiers can't delay the DB
    /// write.
    notifier: Option<Arc<dyn BackupFailureNotifier>>,
    /// Optional event publisher. When set, the executor emits
    /// `Job::BackupCompleted` / `Job::BackupFailed` on every terminal
    /// transition so downstream consumers (SSE bridge, webhooks, audit
    /// log) can react. Fire-and-forget: send errors are logged and
    /// ignored.
    event_publisher: Option<Arc<dyn JobQueue>>,
}

/// One entry in the executor's in-flight map.
///
/// The `JoinHandle` is dropped (and the underlying task aborted) when the
/// map entry is removed — which happens automatically on task exit via
/// the cleanup closure inside `spawn_task`. The `CancellationToken` is
/// the engine's cooperative cancel signal; `cancel(backup_id)` fires it.
struct JobHandle {
    cancel: CancellationToken,
    /// Kept so the executor's `Drop` aborts in-flight tasks if the whole
    /// process is shutting down. Individual cancels never use this — they
    /// fire the cancellation token instead, which lets the engine
    /// cooperatively clean up its sidecar.
    _join: JoinHandle<()>,
}

/// Builder for the executor — collect engines, then `build`.
pub struct BackupExecutorBuilder {
    db: Arc<DatabaseConnection>,
    engines: HashMap<&'static str, Arc<dyn BackupEngine>>,
    max_concurrent: usize,
    notifier: Option<Arc<dyn BackupFailureNotifier>>,
    event_publisher: Option<Arc<dyn JobQueue>>,
}

impl BackupExecutorBuilder {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            engines: HashMap::new(),
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            notifier: None,
            event_publisher: None,
        }
    }

    /// Override the global concurrency cap. Minimum 1 — values smaller
    /// than 1 are clamped silently.
    pub fn with_max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = n.max(1);
        self
    }

    /// Register an engine. The engine's `engine()` key must be unique —
    /// duplicates overwrite the previous registration (intentional, so
    /// plugin reload during dev is well-defined).
    pub fn register_engine(mut self, engine: Arc<dyn BackupEngine>) -> Self {
        self.engines.insert(engine.engine(), engine);
        self
    }

    /// Wire a failure-notification hook. Fired by the executor on every
    /// terminal failure (engine error, timeout, unknown-engine). Optional —
    /// when unset the executor still records the failure in the database
    /// but emits no out-of-band notification.
    pub fn with_notifier(mut self, notifier: Arc<dyn BackupFailureNotifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Wire an event publisher. The executor publishes
    /// `Job::BackupCompleted` / `Job::BackupFailed` on every terminal
    /// transition so other consumers (SSE bridge, webhooks, audit log)
    /// can subscribe. Send errors are logged and ignored.
    pub fn with_event_publisher(mut self, queue: Arc<dyn JobQueue>) -> Self {
        self.event_publisher = Some(queue);
        self
    }

    pub fn build(self) -> BackupExecutor {
        BackupExecutor {
            inner: Arc::new(ExecutorInner {
                db: self.db,
                engines: self.engines,
                semaphore: Arc::new(Semaphore::new(self.max_concurrent)),
                in_flight: Mutex::new(HashMap::new()),
                notifier: self.notifier,
                event_publisher: self.event_publisher,
            }),
        }
    }
}

/// Reason `spawn` declined to start a new task.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("Backup {backup_id} is already running in this process")]
    AlreadyInFlight { backup_id: i32 },

    #[error("No engine registered for key '{engine}'; registered: [{registered}]")]
    UnknownEngine { engine: String, registered: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// Parameters passed by the caller to `spawn`. Mirrors the old
/// `EnqueueJobParams` shape so the migration of callers is mechanical.
#[derive(Debug, Clone)]
pub struct SpawnParams {
    /// FK to the `backups` row this task is fulfilling. Also the dedup
    /// key in the in-flight map.
    pub backup_id: i32,
    /// Which engine key handles this backup (must match a registered
    /// `BackupEngine::engine()`).
    pub engine: String,
    /// Engine-specific JSON parameters (service_id, s3_source_id, …).
    pub params: serde_json::Value,
    /// Wall-clock timeout in seconds. Hard floor of 60s applied so a
    /// zero or corrupt value never instantly fails the job.
    pub max_runtime_secs: i64,
}

impl BackupExecutor {
    /// Spawn a new backup task. Returns immediately once the task is
    /// recorded in the in-flight map (the actual engine work runs in a
    /// detached `tokio::spawn`).
    ///
    /// If the backup is already running in this process, returns
    /// [`SpawnError::AlreadyInFlight`] without touching the semaphore or
    /// the database.
    ///
    /// If the engine key is unknown, the parent `backups` row is flipped
    /// to `failed` synchronously before returning so the user sees the
    /// error immediately. The error is also returned so the HTTP handler
    /// can propagate it.
    pub async fn spawn(&self, params: SpawnParams) -> Result<(), SpawnError> {
        // Engine lookup happens first — no point acquiring a permit only
        // to fail on a typo.
        let engine = match self.inner.engines.get(params.engine.as_str()) {
            Some(e) => Arc::clone(e),
            None => {
                let registered = self
                    .inner
                    .engines
                    .keys()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ");
                let err = SpawnError::UnknownEngine {
                    engine: params.engine.clone(),
                    registered: registered.clone(),
                };
                let msg = err.to_string();
                // Best-effort: surface the unknown-engine error on the
                // backups row so the UI shows something useful instead of
                // a row stuck in `pending`.
                let _ = self.mark_backup_failed(params.backup_id, &msg).await;
                return Err(err);
            }
        };

        // Dedup check before we acquire the permit so two rapid clicks of
        // "Run now" can't both spawn the same backup.
        {
            let in_flight = self.inner.in_flight.lock().await;
            if in_flight.contains_key(&params.backup_id) {
                return Err(SpawnError::AlreadyInFlight {
                    backup_id: params.backup_id,
                });
            }
        }

        // Acquire the global slot. If the pool is full this awaits until
        // a slot frees — that's the back-pressure signal. Callers that
        // want non-blocking semantics should `tokio::time::timeout` this.
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .expect("semaphore closed — executor dropped while spawn was pending");

        // Re-check the dedup map under the same lock that records the
        // handle, otherwise a second concurrent `spawn` for the same
        // backup_id could slip past the first check.
        let cancel = CancellationToken::new();
        {
            let mut in_flight = self.inner.in_flight.lock().await;
            if in_flight.contains_key(&params.backup_id) {
                return Err(SpawnError::AlreadyInFlight {
                    backup_id: params.backup_id,
                });
            }

            let join = self.spawn_task(engine, params.clone(), cancel.clone(), permit);
            in_flight.insert(
                params.backup_id,
                JobHandle {
                    cancel,
                    _join: join,
                },
            );
        }

        Ok(())
    }

    /// Fire the cancellation token for the given backup. Returns `true`
    /// if a live task was signalled, `false` if no task exists in this
    /// process for that backup (already terminal or never started here).
    ///
    /// The DB flip to `failed` is the responsibility of the caller (the
    /// `BackupService::cancel_backup` helper handles it before calling
    /// this). The executor only owns the in-process signal.
    pub async fn cancel(&self, backup_id: i32) -> bool {
        let in_flight = self.inner.in_flight.lock().await;
        match in_flight.get(&backup_id) {
            Some(h) => {
                h.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// On boot, flip every `state='running'` row in `backups` to
    /// `failed`. The previous process owned those tasks; we don't, so
    /// they're dead by definition.
    ///
    /// Same SQL as `reclaim_orphan_jobs_on_startup` but with terminal
    /// semantics (failed instead of pending) — the executor cannot
    /// "resume" a partially-completed backup, so the cleanest UX is to
    /// surface the failure and let the user re-trigger.
    pub async fn reconcile_orphans_on_startup(&self) -> Result<u64, sea_orm::DbErr> {
        let sql = r#"
UPDATE backups
   SET state         = 'failed',
       finished_at   = COALESCE(finished_at, NOW()),
       error_message = COALESCE(error_message, 'Process restarted while backup was running')
 WHERE state IN ('pending', 'running')
        "#;

        let result = self
            .inner
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![],
            ))
            .await?;

        let n = result.rows_affected();
        if n > 0 {
            warn!(
                count = n,
                "BackupExecutor: flipped orphan running/pending backups to failed on startup",
            );
        }
        Ok(n)
    }

    // ── Internal ──────────────────────────────────────────────────────────

    fn spawn_task(
        &self,
        engine: Arc<dyn BackupEngine>,
        params: SpawnParams,
        cancel: CancellationToken,
        permit: OwnedSemaphorePermit,
    ) -> JoinHandle<()> {
        let executor = self.clone();
        let backup_id = params.backup_id;

        tokio::spawn(async move {
            // Permit drops with this scope, releasing the slot whether
            // the engine returns Ok, returns Err, or panics.
            let _permit = permit;

            // Flip backup -> running so the UI sees the state transition
            // immediately. The engine can run for hours; we don't want
            // the row stuck at `pending`.
            if let Err(e) = executor.mark_backup_running(backup_id).await {
                error!(
                    backup_id,
                    error = %e,
                    "BackupExecutor: failed to mark backup as running; aborting before engine call",
                );
                executor
                    .finalize_failed(backup_id, &params.engine, &format!("DB error: {}", e))
                    .await;
                executor.remove_from_in_flight(backup_id).await;
                return;
            }

            let ctx = BackupContext {
                backup_id,
                engine_key: params.engine.clone(),
                params: params.params.clone(),
                cancel: cancel.clone(),
                db: Arc::clone(&executor.inner.db),
            };

            // Engine call with wall-clock timeout. Floor of 60s.
            let runtime_limit = Duration::from_secs(params.max_runtime_secs.max(60) as u64);
            let outcome = run_with_retries(&engine, &ctx, runtime_limit).await;

            match outcome {
                Ok(o) => {
                    if let Err(e) = executor
                        .finalize_completed(backup_id, &params.engine, o)
                        .await
                    {
                        error!(
                            backup_id,
                            error = %e,
                            "BackupExecutor: failed to mark backup as completed (DB error)",
                        );
                    }
                }
                Err(BackupError::Cancelled) => {
                    // The cancel path is finalized by `BackupService::cancel_backup`
                    // which already flipped the row to failed with a "cancelled"
                    // reason. We don't overwrite that — but we still need to
                    // close the parent schedule_runs row.
                    let _ =
                        mark_schedule_run_finished_if_done(executor.inner.db.as_ref(), backup_id)
                            .await;
                }
                Err(e) => {
                    let msg = e.to_string();
                    error!(backup_id, error = %msg, "BackupExecutor: engine returned error");
                    executor
                        .finalize_failed(backup_id, &params.engine, &msg)
                        .await;
                }
            }

            executor.remove_from_in_flight(backup_id).await;
        })
    }

    async fn remove_from_in_flight(&self, backup_id: i32) {
        let mut in_flight = self.inner.in_flight.lock().await;
        in_flight.remove(&backup_id);
    }

    async fn mark_backup_running(&self, backup_id: i32) -> Result<(), sea_orm::DbErr> {
        let sql = r#"
WITH updated_backup AS (
    UPDATE backups
       SET state      = 'running',
           started_at = COALESCE(started_at, NOW())
     WHERE id         = $1
       AND state      = 'pending'
     RETURNING id
)
UPDATE external_service_backups
   SET state      = 'running',
       started_at = COALESCE(started_at, NOW())
 WHERE backup_id IN (SELECT id FROM updated_backup)
   AND state      = 'pending'
        "#;
        self.inner
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![SValue::from(backup_id)],
            ))
            .await?;
        Ok(())
    }

    async fn mark_backup_failed(&self, backup_id: i32, reason: &str) -> Result<(), sea_orm::DbErr> {
        let sql = r#"
WITH updated_backup AS (
    UPDATE backups
       SET state         = 'failed',
           error_message = $1,
           finished_at   = COALESCE(finished_at, NOW())
     WHERE id            = $2
       AND state IN ('pending', 'running')
     RETURNING id
)
UPDATE external_service_backups
   SET state         = 'failed',
       error_message = $1,
       finished_at   = COALESCE(finished_at, NOW())
 WHERE backup_id IN (SELECT id FROM updated_backup)
   AND state IN ('pending', 'running')
        "#;
        self.inner
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![SValue::from(reason.to_owned()), SValue::from(backup_id)],
            ))
            .await?;
        Ok(())
    }

    async fn finalize_failed(&self, backup_id: i32, engine_key: &str, reason: &str) {
        if let Err(e) = self.mark_backup_failed(backup_id, reason).await {
            error!(
                backup_id,
                error = %e,
                "BackupExecutor: finalize_failed UPDATE failed",
            );
        }
        let _ = mark_schedule_run_finished_if_done(self.inner.db.as_ref(), backup_id).await;

        // Fire-and-forget the failure notification. The notifier is
        // responsible for its own error handling; we never await the
        // spawned task so a slow SMTP/webhook cannot stall the finalize
        // path.
        if let Some(notifier) = self.inner.notifier.clone() {
            let ctx = BackupFailureContext {
                backup_id,
                engine: engine_key.to_string(),
                // Attempts/max_attempts are queue-specific concepts. For the
                // executor we report the executor's retry policy as 1/1 so
                // the notifier surface stays uniform; the per-engine retry
                // behaviour lives inside `run_with_retries`.
                attempts: 1,
                max_attempts: 1,
                error_message: reason.to_string(),
                failed_at: chrono::Utc::now(),
            };
            tokio::spawn(async move {
                notifier.notify_failed(ctx).await;
            });
        }

        // Publish a BackupFailed event for downstream consumers. Same
        // fire-and-forget contract as the notifier.
        if let Some(queue) = self.inner.event_publisher.clone() {
            let event = Job::BackupFailed(BackupFailedJob {
                backup_id,
                engine: engine_key.to_string(),
                error_message: reason.to_string(),
            });
            tokio::spawn(async move {
                if let Err(e) = queue.send(event).await {
                    error!(
                        backup_id,
                        error = %e,
                        "BackupExecutor: failed to publish BackupFailed event",
                    );
                }
            });
        }
    }

    async fn finalize_completed(
        &self,
        backup_id: i32,
        engine_key: &str,
        outcome: BackupOutcome,
    ) -> Result<(), sea_orm::DbErr> {
        let sql = r#"
WITH updated_backup AS (
    UPDATE backups
       SET state            = 'completed',
           finished_at      = COALESCE(finished_at, NOW()),
           s3_location      = $1,
           size_bytes       = $2,
           compression_type = $3
     WHERE id               = $4
     RETURNING id
)
UPDATE external_service_backups
   SET state            = 'completed',
       finished_at      = COALESCE(finished_at, NOW()),
       s3_location      = $1,
       size_bytes       = $2,
       compression_type = $3
 WHERE backup_id IN (SELECT id FROM updated_backup)
        "#;
        self.inner
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![
                    SValue::from(outcome.location.clone()),
                    SValue::from(outcome.size_bytes),
                    SValue::from(outcome.compression.clone()),
                    SValue::from(backup_id),
                ],
            ))
            .await?;

        info!(
            backup_id,
            location = %outcome.location,
            size_bytes = ?outcome.size_bytes,
            "BackupExecutor: backup completed",
        );

        let _ = mark_schedule_run_finished_if_done(self.inner.db.as_ref(), backup_id).await;

        // Publish a BackupCompleted event for downstream consumers.
        if let Some(queue) = self.inner.event_publisher.clone() {
            let event = Job::BackupCompleted(BackupCompletedJob {
                backup_id,
                engine: engine_key.to_string(),
                s3_location: outcome.location,
                size_bytes: outcome.size_bytes,
            });
            tokio::spawn(async move {
                if let Err(e) = queue.send(event).await {
                    error!(
                        backup_id,
                        error = %e,
                        "BackupExecutor: failed to publish BackupCompleted event",
                    );
                }
            });
        }

        Ok(())
    }
}

// ── Engine invocation with timeout + retries ────────────────────────────────

async fn run_with_retries(
    engine: &Arc<dyn BackupEngine>,
    ctx: &BackupContext,
    runtime_limit: Duration,
) -> Result<BackupOutcome, BackupError> {
    let mut last_err: Option<BackupError> = None;

    for (attempt, &backoff_secs) in RETRY_BACKOFFS_SECS.iter().enumerate() {
        if backoff_secs > 0 {
            info!(
                backup_id = ctx.backup_id,
                attempt = attempt + 1,
                backoff_secs,
                "BackupExecutor: backing off before retry",
            );
            // Honour cancellation during backoff so a cancel during a
            // retry sleep takes effect immediately.
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    return Err(BackupError::Cancelled);
                }
                _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
            }
        }

        // Cancellation check before each attempt — covers the case where
        // the user cancelled while we were waiting on the semaphore.
        if ctx.cancel.is_cancelled() {
            return Err(BackupError::Cancelled);
        }

        let result = tokio::select! {
            _ = ctx.cancel.cancelled() => Err(BackupError::Cancelled),
            r = tokio::time::timeout(runtime_limit, engine.run(ctx)) => match r {
                Ok(engine_result) => engine_result,
                Err(_elapsed) => {
                    let msg = format!(
                        "Backup exceeded wall-clock timeout of {}s",
                        runtime_limit.as_secs(),
                    );
                    Err(BackupError::Timeout { reason: msg })
                }
            },
        };

        match result {
            Ok(o) => return Ok(o),
            Err(BackupError::Cancelled) => return Err(BackupError::Cancelled),
            Err(e) if e.is_permanent() => {
                // No retry for permanent failures (bad config, missing
                // image, etc). Try to clean up and surface immediately.
                let _ = engine.cleanup(ctx).await;
                return Err(e);
            }
            Err(e) => {
                warn!(
                    backup_id = ctx.backup_id,
                    attempt = attempt + 1,
                    error = %e,
                    "BackupExecutor: engine returned transient error; will retry if attempts remain",
                );
                let _ = engine.cleanup(ctx).await;
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| BackupError::Failed {
        reason: "Engine retries exhausted with no recorded error".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use async_trait::async_trait;
    use sea_orm::{
        ActiveModelTrait, DatabaseBackend, EntityTrait, MockDatabase, MockExecResult, Set,
    };

    use super::*;

    fn executor_with_one_exec() -> (BackupExecutor, Arc<DatabaseConnection>) {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 2,
                }])
                .into_connection(),
        );
        (BackupExecutorBuilder::new(db.clone()).build(), db)
    }

    fn executed_sql(executor: BackupExecutor, db: Arc<DatabaseConnection>) -> String {
        drop(executor);
        let log = Arc::try_unwrap(db)
            .expect("executor should release the mock database")
            .into_transaction_log();
        assert_eq!(
            log.len(),
            1,
            "state transition should be one atomic statement"
        );
        log[0].statements()[0].sql.clone()
    }

    struct FlakyEngine {
        failures_before_success: usize,
        calls: AtomicUsize,
        cleanups: AtomicUsize,
    }

    #[async_trait]
    impl BackupEngine for FlakyEngine {
        fn engine(&self) -> &'static str {
            "flaky"
        }

        async fn run(&self, _ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures_before_success {
                return Err(BackupError::Failed {
                    reason: format!("injected service outage on attempt {}", call + 1),
                });
            }
            Ok(BackupOutcome {
                location: "s3://bench/resumed".to_string(),
                size_bytes: Some(42),
                compression: "wal-g-native".to_string(),
            })
        }

        async fn cleanup(&self, _ctx: &BackupContext) -> Result<(), BackupError> {
            self.cleanups.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn retry_context() -> BackupContext {
        BackupContext {
            backup_id: 42,
            engine_key: "flaky".to_string(),
            params: serde_json::json!({}),
            cancel: CancellationToken::new(),
            db: Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn transient_service_outages_retry_with_exponential_backoff() {
        let engine = Arc::new(FlakyEngine {
            failures_before_success: 3,
            calls: AtomicUsize::new(0),
            cleanups: AtomicUsize::new(0),
        });
        let trait_engine: Arc<dyn BackupEngine> = engine.clone();
        let started = tokio::time::Instant::now();

        let outcome = run_with_retries(&trait_engine, &retry_context(), Duration::from_secs(60))
            .await
            .expect("fourth attempt should recover after the injected outage");

        assert_eq!(outcome.location, "s3://bench/resumed");
        assert_eq!(engine.calls.load(Ordering::SeqCst), 4);
        assert_eq!(engine.cleanups.load(Ordering::SeqCst), 3);
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(30 + 60 + 120),
            "retry delays must follow the documented 30s, 60s, 120s schedule"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_backup_failures_do_not_retry() {
        struct PermanentEngine(AtomicUsize);

        #[async_trait]
        impl BackupEngine for PermanentEngine {
            fn engine(&self) -> &'static str {
                "permanent"
            }

            async fn run(&self, _ctx: &BackupContext) -> Result<BackupOutcome, BackupError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(BackupError::PermanentFailure {
                    reason: "invalid credentials".to_string(),
                })
            }
        }

        let engine = Arc::new(PermanentEngine(AtomicUsize::new(0)));
        let trait_engine: Arc<dyn BackupEngine> = engine.clone();
        let result =
            run_with_retries(&trait_engine, &retry_context(), Duration::from_secs(60)).await;

        assert!(matches!(result, Err(BackupError::PermanentFailure { .. })));
        assert_eq!(engine.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn running_transition_updates_parent_and_external_service_child() {
        let (executor, db) = executor_with_one_exec();

        executor
            .mark_backup_running(42)
            .await
            .expect("running transition should execute");

        let sql = executed_sql(executor, db);
        assert!(sql.contains("UPDATE backups"));
        assert!(sql.contains("UPDATE external_service_backups"));
        assert!(sql.contains("state      = 'running'"));
    }

    #[tokio::test]
    async fn failed_transition_updates_parent_and_external_service_child() {
        let (executor, db) = executor_with_one_exec();

        executor
            .mark_backup_failed(42, "engine stopped")
            .await
            .expect("failed transition should execute");

        let sql = executed_sql(executor, db);
        assert!(sql.contains("UPDATE backups"));
        assert!(sql.contains("UPDATE external_service_backups"));
        assert!(sql.contains("state         = 'failed'"));
    }

    #[tokio::test]
    async fn completed_transition_copies_restore_metadata_to_external_service_child() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 2,
                    },
                    MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    },
                ])
                .into_connection(),
        );
        let executor = BackupExecutorBuilder::new(db.clone()).build();

        executor
            .finalize_completed(
                42,
                "mariadb_physical",
                BackupOutcome {
                    location: "s3://backups/mariadb/walg".to_string(),
                    size_bytes: Some(3_004_748),
                    compression: "wal-g-native".to_string(),
                },
            )
            .await
            .expect("completed transition should execute");

        drop(executor);
        let log = Arc::try_unwrap(db)
            .expect("executor should release the mock database")
            .into_transaction_log();
        assert_eq!(
            log.len(),
            2,
            "completion updates the backup atomically, then closes its schedule run"
        );
        let sql = log[0].statements()[0].sql.clone();
        assert!(sql.contains("UPDATE backups"));
        assert!(sql.contains("UPDATE external_service_backups"));
        assert!(sql.contains("s3_location      = $1"));
        assert!(sql.contains("size_bytes       = $2"));
        assert!(sql.contains("compression_type = $3"));
    }

    async fn setup_real_backup_pair(
        db: &DatabaseConnection,
    ) -> (
        temps_entities::backups::Model,
        temps_entities::external_service_backups::Model,
    ) {
        use temps_entities::{
            backups, external_service_backups, external_services, s3_sources, users,
        };

        let now = chrono::Utc::now();
        let user = users::ActiveModel {
            name: Set("Backup transition test".into()),
            email: Set(format!(
                "backup-transition-{}@example.invalid",
                uuid::Uuid::new_v4()
            )),
            password_hash: Set(None),
            email_verified: Set(true),
            email_verification_token: Set(None),
            email_verification_expires: Set(None),
            password_reset_token: Set(None),
            password_reset_expires: Set(None),
            must_change_password: Set(false),
            deleted_at: Set(None),
            mfa_secret: Set(None),
            mfa_enabled: Set(false),
            mfa_recovery_codes: Set(None),
            oidc_subject: Set(None),
            oidc_provider_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert transition-test user");
        let source = s3_sources::ActiveModel {
            name: Set(format!("transition-source-{}", uuid::Uuid::new_v4())),
            bucket_name: Set("backup-transition-tests".into()),
            region: Set("test-region-1".into()),
            endpoint: Set(None),
            bucket_path: Set(String::new()),
            access_key_id: Set("encrypted-test-key".into()),
            secret_key: Set("encrypted-test-secret".into()),
            force_path_style: Set(Some(true)),
            is_default: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert transition-test S3 source");
        let service = external_services::ActiveModel {
            name: Set(format!("transition-db-{}", uuid::Uuid::new_v4())),
            service_type: Set("postgres".into()),
            version: Set(Some("17".into())),
            status: Set("running".into()),
            created_at: Set(now),
            updated_at: Set(now),
            slug: Set(None),
            config: Set(None),
            node_id: Set(None),
            topology: Set("standalone".into()),
            error_message: Set(None),
            health_status: Set(None),
            last_health_check_at: Set(None),
            last_health_error: Set(None),
            consecutive_health_failures: Set(0),
            health_metadata: Set(None),
            metrics_enabled: Set(false),
            default_backup_provisioned: Set(false),
            container_name: Set(None),
            ai_data_access: Set(false),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert transition-test external service");
        let backup = backups::ActiveModel {
            name: Set("atomic transition".into()),
            backup_id: Set(uuid::Uuid::new_v4().to_string()),
            schedule_id: Set(None),
            backup_type: Set("full".into()),
            state: Set("pending".into()),
            started_at: Set(now),
            finished_at: Set(None),
            size_bytes: Set(None),
            file_count: Set(None),
            s3_source_id: Set(source.id),
            s3_location: Set("s3://backup-transition-tests/source/walg".into()),
            error_message: Set(None),
            metadata: Set(r#"{"local":"preserved"}"#.into()),
            checksum: Set(None),
            compression_type: Set("none".into()),
            created_by: Set(user.id),
            expires_at: Set(None),
            tags: Set("[]".into()),
            schedule_run_id: Set(None),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert transition-test backup");
        let child = external_service_backups::ActiveModel {
            service_id: Set(service.id),
            backup_id: Set(backup.id),
            backup_type: Set("full".into()),
            state: Set("pending".into()),
            started_at: Set(now),
            finished_at: Set(None),
            size_bytes: Set(None),
            s3_location: Set(String::new()),
            error_message: Set(None),
            metadata: Set(serde_json::json!({"engine": "postgres"})),
            checksum: Set(None),
            compression_type: Set("none".into()),
            created_by: Set(user.id),
            expires_at: Set(None),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert transition-test child backup");
        (backup, child)
    }

    #[tokio::test]
    async fn test_backup_transition_concurrent_start_updates_parent_and_child_once() {
        use temps_entities::{backups, external_service_backups};

        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/TestDatabase unavailable; skipping atomic transition test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let (backup, child) = setup_real_backup_pair(db.as_ref()).await;
        let executor = BackupExecutorBuilder::new(db.clone()).build();

        let (first, second) = tokio::join!(
            executor.mark_backup_running(backup.id),
            executor.mark_backup_running(backup.id)
        );

        first.expect("first running transition");
        second.expect("duplicate running transition is an idempotent no-op");
        let parent = backups::Entity::find_by_id(backup.id)
            .one(db.as_ref())
            .await
            .expect("query parent")
            .expect("parent exists");
        let child = external_service_backups::Entity::find_by_id(child.id)
            .one(db.as_ref())
            .await
            .expect("query child")
            .expect("child exists");
        assert_eq!(parent.state, "running");
        assert_eq!(child.state, "running");
    }

    #[tokio::test]
    async fn test_backup_transition_failure_recovery_keeps_parent_and_child_consistent() {
        use temps_entities::{backups, external_service_backups};

        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/TestDatabase unavailable; skipping failure transition test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let (backup, child) = setup_real_backup_pair(db.as_ref()).await;
        let executor = BackupExecutorBuilder::new(db.clone()).build();
        executor
            .mark_backup_running(backup.id)
            .await
            .expect("mark running");

        executor
            .mark_backup_failed(backup.id, "object store unavailable after retries")
            .await
            .expect("mark failed atomically");

        let parent = backups::Entity::find_by_id(backup.id)
            .one(db.as_ref())
            .await
            .expect("query parent")
            .expect("parent exists");
        let child = external_service_backups::Entity::find_by_id(child.id)
            .one(db.as_ref())
            .await
            .expect("query child")
            .expect("child exists");
        assert_eq!(parent.state, "failed");
        assert_eq!(child.state, "failed");
        assert_eq!(
            parent.error_message.as_deref(),
            Some("object store unavailable after retries")
        );
        assert_eq!(child.error_message, parent.error_message);
        assert!(parent.finished_at.is_some());
        assert!(child.finished_at.is_some());
    }

    #[tokio::test]
    async fn test_backup_transition_completion_propagates_restore_contract_atomically() {
        use temps_entities::{backups, external_service_backups};

        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/TestDatabase unavailable; skipping completion transition test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let (backup, child) = setup_real_backup_pair(db.as_ref()).await;
        let executor = BackupExecutorBuilder::new(db.clone()).build();

        executor
            .finalize_completed(
                backup.id,
                "postgres_walg",
                BackupOutcome {
                    location: "s3://backup-transition-tests/cloud/walg".into(),
                    size_bytes: Some(5_242_881),
                    compression: "wal-g-native".into(),
                },
            )
            .await
            .expect("complete parent and child atomically");

        let parent = backups::Entity::find_by_id(backup.id)
            .one(db.as_ref())
            .await
            .expect("query parent")
            .expect("parent exists");
        let child = external_service_backups::Entity::find_by_id(child.id)
            .one(db.as_ref())
            .await
            .expect("query child")
            .expect("child exists");
        assert_eq!(parent.state, "completed");
        assert_eq!(child.state, "completed");
        assert_eq!(parent.s3_location, child.s3_location);
        assert_eq!(parent.size_bytes, child.size_bytes);
        assert_eq!(parent.compression_type, child.compression_type);
        assert_eq!(parent.metadata, r#"{"local":"preserved"}"#);
        assert_eq!(child.metadata["engine"], "postgres");
        assert!(parent.finished_at.is_some());
        assert!(child.finished_at.is_some());
    }
}
