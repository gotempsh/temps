// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Failure-notification hook for the backup runner (ADR-014 deliverable 3).
//!
//! `BackupRunner` holds an optional `Arc<dyn BackupFailureNotifier>`. When a job
//! reaches the terminal `failed` state, the runner fires-and-forgets the notifier
//! via a detached `tokio::spawn` so that slow transports (SMTP) never delay the
//! queue write.
//!
//! ## Dependency isolation
//!
//! This trait lives in `temps-backup-core` so the runner can construct and invoke
//! notifications without depending on `temps-notifications` or `temps-backup`.
//! The concrete implementation (`BackupNotificationAdapter`) lives in
//! `temps-backup`, where `NotificationService` and the schedule/service lookups
//! are available.

use chrono::{DateTime, Utc};

/// Context passed to [`BackupFailureNotifier::notify_failed`] when a job
/// reaches the terminal `failed` state.
///
/// Includes enough information to compose a useful notification without
/// requiring the implementation to perform additional DB lookups for the core
/// fields. Implementations that need richer context (schedule name, service
/// name) may perform their own lookups using `backup_id`.
#[derive(Debug, Clone)]
pub struct BackupFailureContext {
    /// ID of the parent `backups` row.
    pub backup_id: i32,
    /// Engine key that was running when the failure occurred (e.g. `"redis"`).
    pub engine: String,
    /// How many attempts were made (equals `max_attempts` on terminal failure).
    pub attempts: i32,
    /// Maximum allowed attempts for this job.
    pub max_attempts: i32,
    /// The human-readable error message from the engine or runner.
    pub error_message: String,
    /// Wall-clock time at which the job was marked failed.
    pub failed_at: DateTime<Utc>,
}

/// Callback interface for dispatching a notification when a backup job fails
/// permanently (ADR-014 §"Failure notifications").
///
/// ## Contract
///
/// - The method **must not** return an error — implementations log failures
///   internally via `tracing::error!` and swallow them.  A notification
///   dispatch failure must NEVER fail the job-failure write path.
/// - The implementation may perform async I/O (SMTP, webhook) but should be
///   mindful that the runner fires-and-forgets via `tokio::spawn`; it does
///   not await the result.
///
/// ## Object safety
///
/// This trait is object-safe: `Arc<dyn BackupFailureNotifier>` compiles and is
/// how the runner stores the notifier.
#[async_trait::async_trait]
pub trait BackupFailureNotifier: Send + Sync {
    /// Dispatch a notification for a permanently-failed backup job.
    ///
    /// Called once per terminal failure, after the `backup_jobs` and `backups`
    /// rows have been updated.  The call is spawned in a detached task so the
    /// runner does not await it.
    async fn notify_failed(&self, ctx: BackupFailureContext);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Verify that `BackupFailureNotifier` is object-safe — i.e.,
    /// `Arc<dyn BackupFailureNotifier>` compiles without errors.
    ///
    /// If the trait ever gains a non-object-safe method (generic, `Self`, etc.)
    /// this test will stop compiling, alerting the author before the breakage
    /// reaches production code.
    #[tokio::test]
    async fn test_trait_is_object_safe() {
        struct NoopNotifier;

        #[async_trait::async_trait]
        impl BackupFailureNotifier for NoopNotifier {
            async fn notify_failed(&self, _ctx: BackupFailureContext) {}
        }

        // This is the key assertion: `Arc<dyn BackupFailureNotifier>` must compile.
        let _notifier: Arc<dyn BackupFailureNotifier> = Arc::new(NoopNotifier);
    }
}
