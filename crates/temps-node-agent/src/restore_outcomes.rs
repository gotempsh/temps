// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The outcomes of the WAL-G restores this node ran, kept after their
//! requests end.
//!
//! A restore that began writing to its target runs past the request's
//! deadline until it has started the restored data or put the original back
//! (see `run_detached` in `service_data_handlers`). The control plane's
//! request can time out, or its connection drop, before then; the run it
//! tracks must not be marked failed (its credentials not updated, the
//! service released to another restore) while the node is still working.
//! A restore started with an `operation_id` is recorded here, and its
//! outcome is recorded by the detached task itself, so the control plane
//! can ask for it.
//!
//! One entry per service: the latest restore started with an id. Ended
//! entries are dropped after [`RETENTION`], so the map is bounded by the
//! services on this node. Nothing here is on a request hot path.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use temps_service_protocol::{PostgresWalgRestoreProgress, TargetWriteBoundary};

/// How long an ended restore's outcome is kept for its caller to read.
pub(crate) const RETENTION: Duration = Duration::from_secs(24 * 3600);

/// The PostgreSQL WAL-G restores of this node.
pub(crate) static POSTGRES_WALG_RESTORES: LazyLock<RestoreOutcomes> =
    LazyLock::new(RestoreOutcomes::default);

struct Entry {
    operation_id: String,
    writes: Arc<TargetWriteBoundary>,
    ended: Option<(Instant, PostgresWalgRestoreProgress)>,
}

/// Restores by service name; see the module documentation.
#[derive(Default)]
pub(crate) struct RestoreOutcomes {
    entries: Mutex<HashMap<String, Entry>>,
}

impl RestoreOutcomes {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        // Every mutation is a single insert or field write: a panic while
        // holding the lock leaves a valid map.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record that the restore of `service` named `operation_id` started,
    /// writing through `writes`. The returned recorder records its outcome;
    /// dropped without one (the engine panicked, or was stopped at its
    /// deadline), it records that it ended without one.
    pub(crate) fn track(
        &'static self,
        service: &str,
        operation_id: &str,
        writes: Arc<TargetWriteBoundary>,
    ) -> OutcomeRecorder {
        let mut entries = self.lock();
        entries.retain(|_, entry| {
            entry
                .ended
                .as_ref()
                .is_none_or(|(at, _)| at.elapsed() < RETENTION)
        });
        entries.insert(
            service.to_string(),
            Entry {
                operation_id: operation_id.to_string(),
                writes: Arc::clone(&writes),
                ended: None,
            },
        );
        OutcomeRecorder {
            outcomes: self,
            service: service.to_string(),
            operation_id: operation_id.to_string(),
            writes,
            recorded: false,
        }
    }

    /// Where the restore of `service` named `operation_id` is.
    pub(crate) fn progress(
        &self,
        service: &str,
        operation_id: &str,
    ) -> PostgresWalgRestoreProgress {
        let entries = self.lock();
        match entries.get(service) {
            Some(entry) if entry.operation_id == operation_id => match &entry.ended {
                Some((_, progress)) => progress.clone(),
                None => PostgresWalgRestoreProgress::Running {
                    writes_began: entry.writes.writes_began(),
                },
            },
            _ => PostgresWalgRestoreProgress::Unknown,
        }
    }

    fn end(&self, service: &str, operation_id: &str, progress: PostgresWalgRestoreProgress) {
        let mut entries = self.lock();
        // A later restore of the service replaced this one's entry: its
        // outcome is no longer asked for.
        if let Some(entry) = entries.get_mut(service) {
            if entry.operation_id == operation_id {
                entry.ended = Some((Instant::now(), progress));
            }
        }
    }
}

/// Records the outcome of one tracked restore, from its detached task.
pub(crate) struct OutcomeRecorder {
    outcomes: &'static RestoreOutcomes,
    service: String,
    operation_id: String,
    writes: Arc<TargetWriteBoundary>,
    recorded: bool,
}

impl OutcomeRecorder {
    /// Record that the restore ended as `progress` says.
    pub(crate) fn finish(mut self, progress: PostgresWalgRestoreProgress) {
        self.recorded = true;
        self.outcomes
            .end(&self.service, &self.operation_id, progress);
    }

    /// Whether the restore has begun writing to its target.
    pub(crate) fn writes_began(&self) -> bool {
        self.writes.writes_began()
    }
}

impl Drop for OutcomeRecorder {
    fn drop(&mut self) {
        if self.recorded {
            return;
        }
        let writes_began = self.writes.writes_began();
        self.outcomes.end(
            &self.service,
            &self.operation_id,
            PostgresWalgRestoreProgress::Ended {
                writes_began,
                reason: if writes_began {
                    "the restore stopped after it began writing to the database, without an \
                     outcome; the database may be partially restored"
                        .to_string()
                } else {
                    "the restore stopped before it changed the database (it passed its deadline \
                     or stopped unexpectedly)"
                        .to_string()
                },
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_service_protocol::PostgresWalgRestoreResult;

    fn outcomes() -> &'static RestoreOutcomes {
        Box::leak(Box::default())
    }

    #[test]
    fn a_tracked_restore_reports_running_then_its_outcome() {
        let outcomes = outcomes();
        let writes = Arc::new(TargetWriteBoundary::new("orders"));
        let recorder = outcomes.track("orders", "restore-run-7", Arc::clone(&writes));
        assert_eq!(
            outcomes.progress("orders", "restore-run-7"),
            PostgresWalgRestoreProgress::Running {
                writes_began: false
            }
        );
        assert!(writes.begin_target_writes());
        assert_eq!(
            outcomes.progress("orders", "restore-run-7"),
            PostgresWalgRestoreProgress::Running { writes_began: true }
        );
        recorder.finish(PostgresWalgRestoreProgress::Finished {
            outcome: PostgresWalgRestoreResult::Completed,
        });
        assert_eq!(
            outcomes.progress("orders", "restore-run-7"),
            PostgresWalgRestoreProgress::Finished {
                outcome: PostgresWalgRestoreResult::Completed
            }
        );
    }

    #[test]
    fn another_id_or_service_is_unknown() {
        let outcomes = outcomes();
        let _recorder = outcomes.track(
            "orders",
            "restore-run-7",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        assert_eq!(
            outcomes.progress("orders", "restore-run-8"),
            PostgresWalgRestoreProgress::Unknown
        );
        assert_eq!(
            outcomes.progress("billing", "restore-run-7"),
            PostgresWalgRestoreProgress::Unknown
        );
    }

    /// An engine dropped without an outcome (stopped at its deadline, or
    /// panicked) is reported as ended, saying whether it had begun writing.
    #[test]
    fn a_restore_dropped_without_an_outcome_says_whether_it_wrote() {
        let outcomes = outcomes();
        let before = Arc::new(TargetWriteBoundary::new("orders"));
        drop(outcomes.track("orders", "restore-run-1", Arc::clone(&before)));
        assert!(matches!(
            outcomes.progress("orders", "restore-run-1"),
            PostgresWalgRestoreProgress::Ended {
                writes_began: false,
                ..
            }
        ));
        let after = Arc::new(TargetWriteBoundary::new("orders"));
        let recorder = outcomes.track("orders", "restore-run-2", Arc::clone(&after));
        assert!(after.begin_target_writes());
        assert!(recorder.writes_began());
        drop(recorder);
        assert!(matches!(
            outcomes.progress("orders", "restore-run-2"),
            PostgresWalgRestoreProgress::Ended {
                writes_began: true,
                ..
            }
        ));
    }

    /// A later restore of the service replaces the entry; the earlier one's
    /// late outcome does not overwrite it.
    #[test]
    fn a_later_restore_replaces_the_entry() {
        let outcomes = outcomes();
        let first = outcomes.track(
            "orders",
            "restore-run-1",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        let _second = outcomes.track(
            "orders",
            "restore-run-2",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        first.finish(PostgresWalgRestoreProgress::Finished {
            outcome: PostgresWalgRestoreResult::Completed,
        });
        assert_eq!(
            outcomes.progress("orders", "restore-run-1"),
            PostgresWalgRestoreProgress::Unknown
        );
        assert_eq!(
            outcomes.progress("orders", "restore-run-2"),
            PostgresWalgRestoreProgress::Running {
                writes_began: false
            }
        );
    }
}
