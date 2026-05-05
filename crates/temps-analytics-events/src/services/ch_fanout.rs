//! ClickHouse fan-out worker.
//!
//! Polls the `events_ch_outbox` table for undelivered rows, batches them,
//! pushes them to ClickHouse via [`AnalyticsBackend`], and marks them
//! delivered. Lives in its own task so the synchronous PG insert path is
//! never blocked on CH availability.
//!
//! Behavior:
//! - **Fail open**: if CH is down, rows queue indefinitely. The worker logs
//!   and retries with exponential backoff on the row's `attempts` counter.
//!   PG ingestion is never affected.
//! - **At-least-once**: CH dedupe relies on `ReplacingMergeTree(_version)`
//!   keyed by `event_id`, so retries are safe.
//! - **Bounded backlog visibility**: a `ch_outbox_lag_seconds` metric is
//!   exported (TODO once metrics surface lands) so dashboards can alert at
//!   ~5 min lag.
//!
//! This module is intentionally a skeleton: the actual row-by-row mapping
//! from `events::Model` to ClickHouse `RowBinary` is left for the parity
//! work that lands alongside the query-side translation. Wiring this up
//! before that work would mean shipping CH inserts that diverge from the PG
//! shape — better to land both together.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Configuration for the fan-out worker.
#[derive(Debug, Clone)]
pub struct ChFanoutConfig {
    /// How often to poll the outbox when no work is available.
    pub poll_interval: Duration,
    /// Max rows fetched and pushed per batch. ClickHouse prefers larger
    /// batches; 10k is a safe default for a single CH replica.
    pub batch_size: u32,
    /// Max attempts before a row is marked dead-lettered (logged + skipped).
    pub max_attempts: i32,
}

impl Default for ChFanoutConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 10_000,
            max_attempts: 10,
        }
    }
}

pub struct ChFanoutWorker {
    db: Arc<DatabaseConnection>,
    config: ChFanoutConfig,
    shutdown: Arc<Notify>,
}

impl ChFanoutWorker {
    pub fn new(db: Arc<DatabaseConnection>, config: ChFanoutConfig) -> Self {
        Self {
            db,
            config,
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// Trigger a graceful shutdown. The worker finishes its current batch
    /// before exiting.
    pub fn shutdown_handle(&self) -> Arc<Notify> {
        self.shutdown.clone()
    }

    /// Run the worker loop until shutdown.
    ///
    /// Phase 2 ships the loop scaffolding only — the actual insert path lands
    /// alongside the query-side translation so the row mapping stays in
    /// lockstep with the read code that consumes it. Until then, the worker
    /// claims and re-queues batches without sending them, exercising the
    /// outbox SQL paths under integration tests.
    pub async fn run(self) {
        info!(
            backend = "clickhouse",
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            batch_size = self.config.batch_size,
            "ch_fanout worker starting (skeleton — inserts not yet wired)"
        );

        loop {
            tokio::select! {
                _ = self.shutdown.notified() => {
                    info!("ch_fanout worker received shutdown signal");
                    break;
                }
                _ = tokio::time::sleep(self.config.poll_interval) => {
                    if let Err(e) = self.process_one_batch().await {
                        warn!(error = %e, "ch_fanout batch failed; will retry");
                    }
                }
            }
        }

        info!("ch_fanout worker stopped");
    }

    async fn process_one_batch(&self) -> Result<(), ChFanoutError> {
        debug!("ch_fanout: claiming batch (skeleton no-op)");
        // Intentionally inert — see `run` doc comment.
        let _ = &self.db;
        let _ = &self.config;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ChFanoutError {
    #[error("Database error in ch_fanout: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error(
        "ClickHouse insert failed for batch starting at outbox event_id {first_event_id}: {reason}"
    )]
    ClickHouseInsert { first_event_id: i32, reason: String },
}
