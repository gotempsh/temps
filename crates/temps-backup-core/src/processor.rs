// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `BackupJobProcessor`: consumes backup-related `Job` messages from the
//! shared `temps_core::JobQueue` and dispatches engine work to the
//! [`crate::BackupExecutor`].
//!
//! ## Flow
//!
//! 1. HTTP handler / cron / "Run now" inserts a pending `backups` row, then
//!    publishes [`Job::BackupRequested`] via the shared queue. The HTTP
//!    request returns immediately — it never waits for engine work.
//! 2. This processor's `run()` loop subscribes to the queue and matches on
//!    backup-related variants. Other variants are ignored (the queue is
//!    workspace-wide; deployments, certs, etc. share the same channel).
//! 3. On [`Job::BackupRequested`] it calls
//!    [`BackupExecutor::spawn`](crate::BackupExecutor::spawn). The executor
//!    owns the concurrency semaphore + cancel-token map + DB writes.
//! 4. On [`Job::BackupCancelRequested`] it calls
//!    [`BackupExecutor::cancel`](crate::BackupExecutor::cancel).
//! 5. Completion events ([`Job::BackupCompleted`] / [`Job::BackupFailed`])
//!    are published by the executor when an engine terminates; this
//!    processor does not react to them (they exist for downstream
//!    consumers — SSE, webhooks, audit log).
//!
//! ## Why a separate consumer vs. calling `executor.spawn` directly
//!
//! Decoupling the trigger surface from execution lets future producers
//! publish from anywhere (the CLI, a webhook handler, a sidecar) without
//! holding a direct reference to the executor. The trait-based
//! [`temps_core::JobQueue`] also makes the queue backend swappable
//! (currently in-memory tokio broadcast; could be Redis Streams, Postgres
//! LISTEN, etc. without touching engine code).

use std::sync::Arc;

use temps_core::{Job, JobReceiver};
use tracing::{debug, error, info, warn};

use crate::executor::{BackupExecutor, SpawnError, SpawnParams};

/// Errors returned by [`BackupJobProcessor::run`]. The processor's loop is
/// usually run inside a `tokio::spawn` and the only way it exits is the
/// channel closing (e.g. shutdown).
#[derive(Debug, thiserror::Error)]
pub enum BackupJobProcessorError {
    #[error("Job channel closed; processor exiting")]
    ChannelClosed,
}

/// Backup-side queue consumer. Cheap to clone — internal state is
/// `Arc<BackupExecutor>` (itself cheap to clone).
#[derive(Clone)]
pub struct BackupJobProcessor {
    executor: Arc<BackupExecutor>,
}

impl BackupJobProcessor {
    pub fn new(executor: Arc<BackupExecutor>) -> Self {
        Self { executor }
    }

    /// Run the receive-and-dispatch loop. Returns when the queue channel
    /// closes (shutdown) or on an unrecoverable internal error.
    pub async fn run(
        self,
        mut receiver: Box<dyn JobReceiver>,
    ) -> Result<(), BackupJobProcessorError> {
        info!("BackupJobProcessor: starting consumer loop");
        loop {
            let job = match receiver.recv().await {
                Ok(j) => j,
                Err(temps_core::QueueError::ChannelClosed) => {
                    info!("BackupJobProcessor: channel closed, exiting");
                    return Err(BackupJobProcessorError::ChannelClosed);
                }
                Err(e) => {
                    // Broadcast lag / other transient errors: log and keep
                    // going. The next recv() will return the next available
                    // job.
                    warn!("BackupJobProcessor: receive error (continuing): {}", e);
                    continue;
                }
            };

            self.handle_job(job).await;
        }
    }

    async fn handle_job(&self, job: Job) {
        match job {
            Job::BackupRequested(req) => {
                debug!(
                    backup_id = req.backup_id,
                    engine = %req.engine,
                    "BackupJobProcessor: dispatching BackupRequested",
                );
                let params = SpawnParams {
                    backup_id: req.backup_id,
                    engine: req.engine.clone(),
                    params: req.params,
                    max_runtime_secs: req.max_runtime_secs,
                };
                match self.executor.spawn(params).await {
                    Ok(()) => {}
                    Err(SpawnError::AlreadyInFlight { backup_id }) => {
                        // Same-process double-fire. Not an error — log and
                        // ignore so a duplicate publish doesn't fail the
                        // queue loop.
                        debug!(
                            backup_id,
                            "BackupJobProcessor: BackupRequested for already-in-flight task; ignoring",
                        );
                    }
                    Err(SpawnError::UnknownEngine { engine, registered }) => {
                        error!(
                            backup_id = req.backup_id,
                            engine = %engine,
                            registered = %registered,
                            "BackupJobProcessor: BackupRequested for unknown engine; executor already \
                             flipped row to failed",
                        );
                    }
                    Err(SpawnError::Database(e)) => {
                        error!(
                            backup_id = req.backup_id,
                            error = %e,
                            "BackupJobProcessor: spawn failed at DB layer",
                        );
                    }
                }
            }

            Job::BackupCancelRequested(req) => {
                debug!(
                    backup_id = req.backup_id,
                    "BackupJobProcessor: dispatching BackupCancelRequested",
                );
                let signalled = self.executor.cancel(req.backup_id).await;
                if !signalled {
                    debug!(
                        backup_id = req.backup_id,
                        "BackupJobProcessor: cancel ignored — no in-process task for this backup",
                    );
                }
            }

            // Completion events are published by the executor; the
            // processor does not consume its own output. Downstream
            // consumers (notifier adapter, SSE bridge, webhooks) can
            // subscribe separately.
            Job::BackupCompleted(_) | Job::BackupFailed(_) => {}

            // Workspace-wide queue: ignore every non-backup variant.
            _ => {}
        }
    }
}
