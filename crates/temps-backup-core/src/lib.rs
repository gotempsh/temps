// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `temps-backup-core`: shared primitives for the in-process backup executor.
//!
//! Trigger flow:
//!
//! 1. HTTP / cron publishes `Job::BackupRequested` on the shared
//!    `temps_core::JobQueue` (in-memory broadcast today; swappable).
//! 2. [`BackupJobProcessor`] subscribes and dispatches to
//!    [`BackupExecutor::spawn`].
//! 3. Executor owns concurrency, cancel tokens, and DB writes. On terminal
//!    state it publishes `Job::BackupCompleted` / `Job::BackupFailed` for
//!    downstream consumers (SSE, webhooks, notifier adapter).
//!
//! Engines (in `temps-backup`) implement [`engine_v2::BackupEngine`].

pub mod engine_v2;
pub mod executor;
pub mod notifier;
pub mod processor;
pub mod queue;

pub use engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
pub use executor::{BackupExecutor, BackupExecutorBuilder, SpawnError, SpawnParams};
pub use notifier::{BackupFailureContext, BackupFailureNotifier};
pub use processor::{BackupJobProcessor, BackupJobProcessorError};
pub use queue::{
    cancel_backup, cancel_schedule_run, mark_schedule_run_finished_if_done, BackupQueueError,
};
