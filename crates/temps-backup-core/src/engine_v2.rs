// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Simplified `BackupEngine` trait for the in-process executor.
//!
//! Replaces the old multi-step `BackupEngine` trait (still in `engine.rs`
//! for now during the migration). Differences:
//!
//! - **One async fn.** `run(&self, ctx) -> Result<BackupOutcome, BackupError>`
//!   replaces `execute(ctx, cursor) -> BoxStream<StepEvent>`. Each engine
//!   runs start-to-finish in a single call. No step state machine.
//! - **No durable_state.** Engine-internal step recovery is dropped —
//!   retries restart from scratch. This is fine for backups (pg_dumpall
//!   is naturally idempotent against the same source DB; wal-g handles
//!   its own incremental state; s3_mirror is `--overwrite`).
//! - **Cooperative cancel + heartbeats are pull-based.** The engine
//!   checks `ctx.cancel.is_cancelled()` at await points and yields
//!   `tokio::select!` against `ctx.cancel.cancelled()` when waiting on
//!   long-running I/O. No mpsc heartbeat channel; the executor's
//!   wall-clock `tokio::time::timeout` is the deadline.
//! - **Cleanup is explicit.** `cleanup(&self, ctx)` is called by the
//!   executor on any error path. The engine is responsible for reaping
//!   its sidecar container, deleting partial S3 objects, etc.
//!
//! Engine implementations should use the RAII `SidecarGuard` helper
//! (`temps-backup` crate) so a panic or early return still removes any
//! containers they started.

use std::sync::Arc;

use sea_orm::DatabaseConnection;
use tokio_util::sync::CancellationToken;

/// Per-task context passed to `BackupEngine::run`. Carries the
/// identifying ids, engine-specific JSON params, the cancel signal, and
/// a shared DB handle for engines that need to look up service or
/// s3-source rows.
#[derive(Clone)]
pub struct BackupContext {
    /// FK to the `backups` row this task is executing.
    pub backup_id: i32,
    /// Engine key — same as `BackupEngine::engine()`. Mostly useful for
    /// logging.
    pub engine_key: String,
    /// Engine-specific parameters (e.g. `{"service_id": 28, "s3_source_id": 2}`).
    pub params: serde_json::Value,
    /// Cooperative cancel signal. Engines must check this at every await
    /// point. The executor fires it for manual cancels, wall-clock
    /// timeouts, and shutdown.
    pub cancel: CancellationToken,
    /// Shared sea-orm connection. Engines load services, s3_sources, and
    /// related rows through this.
    pub db: Arc<DatabaseConnection>,
}

/// Successful backup output. The executor writes these fields onto the
/// `backups` row when the engine returns `Ok`.
#[derive(Debug, Clone)]
pub struct BackupOutcome {
    /// S3 URL or object key where the backup data lives.
    pub location: String,
    /// Final size in bytes once uploaded.
    pub size_bytes: Option<i64>,
    /// Compression marker ("gzip", "lz4", "none"). Written to
    /// `backups.compression_type`.
    pub compression: String,
}

/// Error variants returned by `BackupEngine::run`.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The engine ran to a known failure state and there's no point
    /// retrying with the same inputs. Bad config, missing image, etc.
    /// The executor skips backoff and surfaces this immediately.
    #[error("Backup failed permanently: {reason}")]
    PermanentFailure { reason: String },

    /// Transient error — network blip, Docker daemon hiccup, S3 5xx.
    /// The executor retries up to its configured cap.
    #[error("Backup failed: {reason}")]
    Failed { reason: String },

    /// `ctx.cancel` was fired mid-run. The executor recognises this
    /// variant and skips the failure flip on the parent backups row
    /// (the cancel handler already wrote the user-facing reason).
    #[error("Backup cancelled by user")]
    Cancelled,

    /// The wall-clock deadline elapsed inside the executor's
    /// `tokio::time::timeout`. The engine never sees this — the
    /// executor synthesises it after the timeout fires.
    #[error("Backup exceeded wall-clock timeout: {reason}")]
    Timeout { reason: String },
}

impl BackupError {
    /// Whether retry is pointless for this variant.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            BackupError::PermanentFailure { .. } | BackupError::Timeout { .. }
        )
    }
}

/// The simplified engine trait. One run, one cleanup, no state machine.
#[async_trait::async_trait]
pub trait BackupEngine: Send + Sync {
    /// Engine key. Must match the value stored in `backup_jobs.engine`
    /// (e.g. "postgres_pgdump", "redis", "control_plane"). Must be
    /// unique within the executor's registry.
    fn engine(&self) -> &'static str;

    /// Execute the backup end-to-end. Engines are expected to:
    ///
    /// 1. Pull and start any sidecar container they need (using an RAII
    ///    guard so it gets reaped on any return).
    /// 2. Run their backup command(s), polling `ctx.cancel` at await
    ///    points.
    /// 3. Upload the result to S3.
    /// 4. Return the final `BackupOutcome`.
    ///
    /// Idempotence on retry is the engine's responsibility — but the
    /// executor calls `cleanup` between retries, so engines that can
    /// fully reset their state in `cleanup` need no further work.
    async fn run(&self, ctx: &BackupContext) -> Result<BackupOutcome, BackupError>;

    /// Best-effort cleanup. Called by the executor before a retry, and
    /// after a permanent failure or cancel. Engines should remove any
    /// container they started, drop partial S3 objects, etc. Errors are
    /// logged and ignored — the next retry doesn't depend on cleanup
    /// success.
    async fn cleanup(&self, _ctx: &BackupContext) -> Result<(), BackupError> {
        // Default no-op for engines that don't need cleanup (e.g. an
        // engine that does all its work inside one atomic API call).
        Ok(())
    }
}
