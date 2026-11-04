// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The in-process backup executor: the `BackupEngine` trait, the job
//! processor, the concurrency-limited executor and the failure notifier.
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
//! Engines (in [`crate::engines`]) implement [`engine_v2::BackupEngine`].

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod engine_v2;
pub(crate) mod executor;
pub(crate) mod notifier;
pub(crate) mod processor;
pub(crate) mod queue;

pub use engine_v2::{BackupContext, BackupEngine, BackupError, BackupOutcome};
pub(crate) use executor::{BackupExecutor, BackupExecutorBuilder};
pub(crate) use notifier::{BackupFailureContext, BackupFailureNotifier};
pub(crate) use processor::BackupJobProcessor;
pub(crate) use queue::{cancel_backup, cancel_schedule_run};
