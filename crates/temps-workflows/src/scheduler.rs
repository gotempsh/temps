// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The scheduler (ADR-103 §6): claims due runs and hands them to a
//! fixed-size invoker pool; reaps expired leases; reconciles interrupted
//! event deliveries; flushes metrics; applies retention.
//!
//! **Load and memory.** Work is pulled, never pushed: the dispatcher claims
//! only as many runs as the pool has free slots ([`SchedulerConfig::invokers`],
//! 16 by default), so at saturation runs wait in the table, not in memory,
//! and the queue-depth metric shows the backlog. A claim costs one bounded
//! query. The dispatcher wakes on `NOTIFY workflow_runs_due` (a run started,
//! woken or retried now) and otherwise every [`SchedulerConfig::poll_interval`],
//! which also covers a dropped LISTEN connection. Maintenance passes are
//! bounded batches.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;

use crate::engine::WorkflowEngine;
use crate::payload::PayloadScope;
use crate::store::{WorkflowStore, DUE_CHANNEL};

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Invocations in flight at once in this process.
    pub invokers: usize,
    /// Most runs claimed by one query.
    pub claim_batch: u64,
    pub poll_interval: Duration,
    pub maintenance_interval: Duration,
    pub reconcile_interval: Duration,
    pub metrics_interval: Duration,
    pub storage_interval: Duration,
    pub retention_interval: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            invokers: 16,
            claim_batch: 32,
            poll_interval: Duration::from_secs(5),
            maintenance_interval: Duration::from_secs(15),
            reconcile_interval: Duration::from_secs(60),
            metrics_interval: crate::metrics::FLUSH_INTERVAL,
            storage_interval: Duration::from_secs(15 * 60),
            retention_interval: Duration::from_secs(60 * 60),
        }
    }
}

const REAP_BATCH: u64 = 200;
const RECONCILE_BATCH: u64 = 100;
const RETENTION_BATCH: u64 = 500;
const RETENTION_ROUNDS: usize = 10;
const EVENT_RETENTION_BATCH: u64 = 5_000;
const LISTEN_RECONNECT: Duration = Duration::from_secs(5);
/// A LISTEN socket can die silently; reconnect when idle this long.
const LISTEN_IDLE: Duration = Duration::from_secs(120);

pub struct WorkflowScheduler {
    engine: Arc<WorkflowEngine>,
    store: Arc<dyn WorkflowStore>,
    codec: Arc<crate::payload::PayloadCodec>,
    metrics: Arc<crate::metrics::WorkflowMetrics>,
    listen_pool: Option<sqlx::PgPool>,
    config: SchedulerConfig,
    wake: Arc<Notify>,
    pool: Arc<Semaphore>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl WorkflowScheduler {
    pub fn new(
        engine: Arc<WorkflowEngine>,
        store: Arc<dyn WorkflowStore>,
        codec: Arc<crate::payload::PayloadCodec>,
        metrics: Arc<crate::metrics::WorkflowMetrics>,
        listen_pool: Option<sqlx::PgPool>,
        config: SchedulerConfig,
    ) -> Self {
        let pool = Arc::new(Semaphore::new(config.invokers.max(1)));
        Self {
            engine,
            store,
            codec,
            metrics,
            listen_pool,
            config,
            wake: Arc::new(Notify::new()),
            pool,
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// Start the background loops. Calling it twice is a no-op.
    pub fn start(self: &Arc<Self>) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !tasks.is_empty() {
            return;
        }
        let dispatcher = self.clone();
        tasks.push(tokio::spawn(
            async move { dispatcher.dispatch_loop().await },
        ));
        let maintenance = self.clone();
        tasks.push(tokio::spawn(
            async move { maintenance.maintenance_loop().await },
        ));
        if let Some(pool) = self.listen_pool.clone() {
            let wake = self.wake.clone();
            tasks.push(tokio::spawn(async move { listen_loop(pool, wake).await }));
        }
        tracing::info!(
            invokers = self.config.invokers,
            poll_secs = self.config.poll_interval.as_secs(),
            "workflow scheduler started"
        );
    }

    /// Stop the loops. In-flight invocations finish on their own; their
    /// leases protect the runs if the process exits first.
    pub fn shutdown(&self) {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for task in tasks.drain(..) {
            task.abort();
        }
    }

    /// Wake the dispatcher now (this process).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    async fn dispatch_loop(self: Arc<Self>) {
        loop {
            self.dispatch().await;
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(self.config.poll_interval) => {}
            }
        }
    }

    /// Claim and start as many due runs as there are free invoker slots.
    /// Returns how many were started.
    /// No invocation is in flight (tests drive `dispatch` until idle).
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.pool.available_permits() == self.config.invokers
    }

    pub async fn dispatch(self: &Arc<Self>) -> usize {
        let mut started = 0;
        loop {
            let free = self.pool.available_permits();
            if free == 0 {
                return started;
            }
            let batch = (free as u64).min(self.config.claim_batch);
            // Reserve the slots before claiming, so a claimed run always has one.
            let Ok(mut reserved) = self
                .pool
                .clone()
                .try_acquire_many_owned(u32::try_from(batch).unwrap_or(u32::MAX))
            else {
                return started;
            };
            let claims = match self.engine.claim(batch).await {
                Ok(claims) => claims,
                Err(error) => {
                    tracing::error!(%error, "claiming due workflow runs failed");
                    return started;
                }
            };
            let claimed = claims.len() as u64;
            for claim in claims {
                let Some(permit) = reserved.split(1) else {
                    // Unreachable: no more runs are claimed than reserved.
                    // The run's lease expiry gives it back.
                    tracing::error!(run_id = %claim.run.id, "claimed a workflow run without a free invoker slot");
                    continue;
                };
                let engine = self.engine.clone();
                let wake = self.wake.clone();
                tokio::spawn(async move {
                    engine.invoke(claim).await;
                    drop(permit);
                    // A slot freed: more due runs may be waiting.
                    wake.notify_one();
                });
                started += 1;
            }
            // Unused reservations go back to the pool here.
            drop(reserved);
            if claimed < batch {
                return started;
            }
        }
    }

    async fn maintenance_loop(self: Arc<Self>) {
        let mut last_reconcile = Instant::now();
        let mut last_metrics = Instant::now();
        let mut last_storage: Option<Instant> = None;
        let mut last_retention: Option<Instant> = None;
        let mut interval = tokio::time::interval(self.config.maintenance_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.reap().await;
            if last_reconcile.elapsed() >= self.config.reconcile_interval {
                last_reconcile = Instant::now();
                if let Err(error) = self.engine.reconcile_waits(RECONCILE_BATCH).await {
                    tracing::warn!(%error, "reconciling workflow event deliveries failed");
                }
                if let Err(error) = self.engine.reconcile_children(RECONCILE_BATCH).await {
                    tracing::warn!(%error, "reconciling child workflow results failed");
                }
            }
            if last_metrics.elapsed() >= self.config.metrics_interval {
                last_metrics = Instant::now();
                let with_storage =
                    last_storage.is_none_or(|at| at.elapsed() >= self.config.storage_interval);
                if with_storage {
                    last_storage = Some(Instant::now());
                }
                self.flush_metrics(with_storage).await;
            }
            if last_retention.is_none_or(|at| at.elapsed() >= self.config.retention_interval) {
                last_retention = Some(Instant::now());
                self.apply_retention().await;
            }
        }
    }

    /// Requeue or park runs whose lease expired.
    pub async fn reap(&self) -> usize {
        match self.store.reap_expired(REAP_BATCH).await {
            Ok(reaped) => {
                for run in &reaped {
                    self.metrics.invocation_failed(
                        run.project_id,
                        run.environment_id,
                        &run.workflow_name,
                        "lease_expired",
                    );
                    tracing::warn!(
                        run_id = %run.run_id,
                        project_id = run.project_id,
                        environment_id = run.environment_id,
                        status = run.status.as_str(),
                        "workflow run lease expired without a heartbeat"
                    );
                }
                if !reaped.is_empty() {
                    self.wake();
                }
                reaped.len()
            }
            Err(error) => {
                tracing::error!(%error, "reaping expired workflow leases failed");
                0
            }
        }
    }

    pub async fn flush_metrics(&self, with_storage: bool) {
        let queues = match self.store.queue_stats().await {
            Ok(queues) => queues,
            Err(error) => {
                tracing::warn!(%error, "could not measure workflow queues");
                Vec::new()
            }
        };
        let storage = if with_storage {
            match self.store.journal_sizes().await {
                Ok(sizes) => Some(sizes),
                Err(error) => {
                    tracing::warn!(%error, "could not measure workflow storage");
                    None
                }
            }
        } else {
            None
        };
        self.metrics.flush(&queues, storage.as_deref()).await;
    }

    /// Delete finished runs (and their blobs) and events past retention.
    pub async fn apply_retention(&self) -> u64 {
        let mut deleted = 0;
        for _ in 0..RETENTION_ROUNDS {
            // Rows first, blobs after: a blob left behind by a failed delete
            // is only storage, while a blob deleted under a run that is still
            // live (retried meanwhile) would break its replay.
            let expired = match self.store.delete_expired_runs(RETENTION_BATCH).await {
                Ok(expired) => expired,
                Err(error) => {
                    tracing::warn!(%error, "deleting expired workflow runs failed");
                    break;
                }
            };
            deleted += expired.len() as u64;
            for run in expired.iter().filter(|run| run.has_blob_payloads) {
                self.codec
                    .delete_run_blobs(&PayloadScope {
                        project_id: run.project_id,
                        environment_id: run.environment_id,
                        run_id: run.run_id,
                    })
                    .await;
            }
            if (expired.len() as u64) < RETENTION_BATCH {
                break;
            }
        }
        let events = self
            .store
            .delete_expired_events(EVENT_RETENTION_BATCH)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "deleting expired workflow events failed");
                0
            });
        if deleted > 0 || events > 0 {
            tracing::info!(
                runs = deleted,
                events,
                "workflow retention deleted expired runs and events"
            );
        }
        deleted
    }
}

impl Drop for WorkflowScheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn listen_loop(pool: sqlx::PgPool, wake: Arc<Notify>) {
    loop {
        let mut listener = match sqlx::postgres::PgListener::connect_with(&pool).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "workflow scheduler could not open its LISTEN connection; polling meanwhile");
                tokio::time::sleep(LISTEN_RECONNECT).await;
                continue;
            }
        };
        if let Err(error) = listener.listen(DUE_CHANNEL).await {
            tracing::warn!(%error, "workflow scheduler could not LISTEN; polling meanwhile");
            tokio::time::sleep(LISTEN_RECONNECT).await;
            continue;
        }
        tracing::debug!(channel = DUE_CHANNEL, "workflow scheduler listening");
        loop {
            match tokio::time::timeout(LISTEN_IDLE, listener.recv()).await {
                Ok(Ok(_)) => wake.notify_one(),
                // Idle: reconnect in case the socket died silently.
                Err(_) => break,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "workflow scheduler lost its LISTEN connection; reconnecting");
                    tokio::time::sleep(LISTEN_RECONNECT).await;
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use temps_core::NoopProjectMetricsSink;

    use super::*;
    use crate::engine::tests::{harness, Harness, Reply, Script};
    use crate::protocol::Outcome;

    fn scheduler(
        h: &Harness,
        invokers: usize,
        listen: bool,
        poll: Duration,
    ) -> Arc<WorkflowScheduler> {
        Arc::new(WorkflowScheduler::new(
            h.engine.clone(),
            h.store.clone(),
            h.codec.clone(),
            Arc::new(crate::metrics::WorkflowMetrics::new(Arc::new(
                NoopProjectMetricsSink,
            ))),
            listen.then(|| h.env.conn.get_postgres_connection_pool().clone()),
            SchedulerConfig {
                invokers,
                poll_interval: poll,
                ..SchedulerConfig::default()
            },
        ))
    }

    async fn wait_until(what: &str, mut done: impl AsyncFnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if done().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn the_invoker_pool_bounds_invocations_in_flight() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let script: Script = {
            let in_flight = in_flight.clone();
            let peak = peak.clone();
            Arc::new(move |_, _, _| {
                let in_flight = in_flight.clone();
                let peak = peak.clone();
                Box::pin(async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Reply::Outcome(Outcome::Completed { output: None })
                })
            })
        };
        let Some(h) = harness("pool test", &["batch"], script, true).await else {
            return;
        };
        let mut ids = Vec::new();
        for _ in 0..6 {
            ids.push(h.start("batch", json!(null)).await);
        }
        let scheduler = scheduler(&h, 2, false, Duration::from_millis(100));
        // One dispatch fills the pool and no more.
        assert_eq!(scheduler.dispatch().await, 2);
        assert_eq!(scheduler.dispatch().await, 0);
        scheduler.start();
        wait_until("every run to complete", async || {
            let mut all = true;
            for id in &ids {
                all &= h.run(*id).await.status == "completed";
            }
            all
        })
        .await;
        assert_eq!(
            peak.load(Ordering::SeqCst),
            2,
            "more invocations ran than the pool allows"
        );
        scheduler.shutdown();
    }

    #[tokio::test]
    async fn a_started_run_is_invoked_on_notify_without_waiting_for_the_poll() {
        let script: Script = Arc::new(|_, _, _| {
            Box::pin(async { Reply::Outcome(Outcome::Completed { output: None }) })
        });
        let Some(h) = harness("notify test", &["fast"], script, true).await else {
            return;
        };
        // The poll alone would take an hour.
        let scheduler = scheduler(&h, 4, true, Duration::from_secs(3600));
        scheduler.start();
        // Let the first (empty) dispatch run and the listener subscribe.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let id = h.start("fast", json!(null)).await;
        wait_until("the run to complete", async || {
            h.run(id).await.status == "completed"
        })
        .await;
        scheduler.shutdown();
    }
}
