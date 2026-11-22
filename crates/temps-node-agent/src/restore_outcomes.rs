// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The outcomes of the worker restores this node ran (PostgreSQL WAL-G,
//! MongoDB, Redis, MariaDB), kept after their requests end.
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
//! One entry per engine and service: the latest restore started with an
//! id. Ended entries are dropped after [`RETENTION`], so the map is bounded
//! by the services on this node. Nothing here is on a request hot path.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use temps_service_protocol::{
    TargetWriteBoundary, WorkerRestoreEngine, WorkerRestoreProgress, WorkerRestoreResult,
};

/// How long an ended restore's outcome is kept for its caller to read.
pub(crate) const RETENTION: Duration = Duration::from_secs(24 * 3600);

/// The worker restores of this node.
pub(crate) static WORKER_RESTORES: LazyLock<RestoreOutcomes> =
    LazyLock::new(RestoreOutcomes::default);

struct Entry {
    operation_id: String,
    writes: Arc<TargetWriteBoundary>,
    ended: Option<(Instant, WorkerRestoreProgress)>,
}

type Key = (WorkerRestoreEngine, String);

/// Restores by engine and service name; see the module documentation.
#[derive(Default)]
pub(crate) struct RestoreOutcomes {
    entries: Mutex<HashMap<Key, Entry>>,
}

impl RestoreOutcomes {
    fn lock(&self) -> MutexGuard<'_, HashMap<Key, Entry>> {
        // Every mutation is a single insert or field write: a panic while
        // holding the lock leaves a valid map.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record that the `engine` restore of `service` named `operation_id`
    /// started, writing through `writes`, when its caller named it (an
    /// unnamed restore's outcome is never asked for). The returned recorder
    /// records its outcome; dropped without one (the engine panicked, or
    /// was stopped at its deadline), it records that it ended without one.
    pub(crate) fn track_named(
        &'static self,
        engine: WorkerRestoreEngine,
        service: &str,
        operation_id: Option<&str>,
        writes: &Arc<TargetWriteBoundary>,
    ) -> Option<OutcomeRecorder> {
        operation_id
            .map(|operation_id| self.track(engine, service, operation_id, Arc::clone(writes)))
    }

    /// [`Self::track_named`] for a named restore.
    pub(crate) fn track(
        &'static self,
        engine: WorkerRestoreEngine,
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
            (engine, service.to_string()),
            Entry {
                operation_id: operation_id.to_string(),
                writes: Arc::clone(&writes),
                ended: None,
            },
        );
        OutcomeRecorder {
            outcomes: self,
            engine,
            service: service.to_string(),
            operation_id: operation_id.to_string(),
            writes,
            recorded: false,
        }
    }

    /// Where the `engine` restore of `service` named `operation_id` is.
    pub(crate) fn progress(
        &self,
        engine: WorkerRestoreEngine,
        service: &str,
        operation_id: &str,
    ) -> WorkerRestoreProgress {
        let entries = self.lock();
        match entries.get(&(engine, service.to_string())) {
            Some(entry) if entry.operation_id == operation_id => match &entry.ended {
                Some((_, progress)) => progress.clone(),
                None => WorkerRestoreProgress::Running {
                    writes_began: entry.writes.writes_began(),
                },
            },
            _ => WorkerRestoreProgress::Unknown,
        }
    }

    fn end(
        &self,
        engine: WorkerRestoreEngine,
        service: &str,
        operation_id: &str,
        progress: WorkerRestoreProgress,
    ) {
        let mut entries = self.lock();
        // A later restore of the service replaced this one's entry: its
        // outcome is no longer asked for.
        if let Some(entry) = entries.get_mut(&(engine, service.to_string())) {
            if entry.operation_id == operation_id {
                entry.ended = Some((Instant::now(), progress));
            }
        }
    }
}

/// Records the outcome of one tracked restore, from its detached task.
pub(crate) struct OutcomeRecorder {
    outcomes: &'static RestoreOutcomes,
    engine: WorkerRestoreEngine,
    service: String,
    operation_id: String,
    writes: Arc<TargetWriteBoundary>,
    recorded: bool,
}

impl OutcomeRecorder {
    /// Record that the restore ended as `progress` says.
    pub(crate) fn finish(mut self, progress: WorkerRestoreProgress) {
        self.recorded = true;
        self.outcomes
            .end(self.engine, &self.service, &self.operation_id, progress);
    }

    /// Record what the engine returned: its outcome, or that it could not
    /// run (`error`).
    pub(crate) fn finish_with<T, E>(self, result: &Result<T, E>)
    where
        T: Clone + Into<WorkerRestoreResult>,
        E: std::fmt::Display,
    {
        let progress = match result {
            Ok(outcome) => WorkerRestoreProgress::Finished {
                outcome: outcome.clone().into(),
            },
            Err(error) => WorkerRestoreProgress::Ended {
                writes_began: self.writes_began(),
                reason: error.to_string(),
            },
        };
        self.finish(progress);
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
            self.engine,
            &self.service,
            &self.operation_id,
            WorkerRestoreProgress::Ended {
                writes_began,
                reason: if writes_began {
                    "the restore stopped after it began writing to the service, without an \
                     outcome; the service may be partially restored"
                        .to_string()
                } else {
                    "the restore stopped before it changed the service (it passed its deadline \
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
    const PG: WorkerRestoreEngine = WorkerRestoreEngine::PostgresWalg;

    fn outcomes() -> &'static RestoreOutcomes {
        Box::leak(Box::default())
    }

    #[test]
    fn a_tracked_restore_reports_running_then_its_outcome() {
        let outcomes = outcomes();
        let writes = Arc::new(TargetWriteBoundary::new("orders"));
        let recorder = outcomes.track(PG, "orders", "restore-run-7", Arc::clone(&writes));
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-7"),
            WorkerRestoreProgress::Running {
                writes_began: false
            }
        );
        assert!(writes.begin_target_writes());
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-7"),
            WorkerRestoreProgress::Running { writes_began: true }
        );
        recorder.finish(WorkerRestoreProgress::Finished {
            outcome: WorkerRestoreResult::Completed,
        });
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-7"),
            WorkerRestoreProgress::Finished {
                outcome: WorkerRestoreResult::Completed
            }
        );
    }

    #[test]
    fn another_id_or_service_is_unknown() {
        let outcomes = outcomes();
        let _recorder = outcomes.track(
            PG,
            "orders",
            "restore-run-7",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-8"),
            WorkerRestoreProgress::Unknown
        );
        assert_eq!(
            outcomes.progress(PG, "billing", "restore-run-7"),
            WorkerRestoreProgress::Unknown
        );
    }

    /// An engine dropped without an outcome (stopped at its deadline, or
    /// panicked) is reported as ended, saying whether it had begun writing.
    #[test]
    fn a_restore_dropped_without_an_outcome_says_whether_it_wrote() {
        let outcomes = outcomes();
        let before = Arc::new(TargetWriteBoundary::new("orders"));
        drop(outcomes.track(PG, "orders", "restore-run-1", Arc::clone(&before)));
        assert!(matches!(
            outcomes.progress(PG, "orders", "restore-run-1"),
            WorkerRestoreProgress::Ended {
                writes_began: false,
                ..
            }
        ));
        let after = Arc::new(TargetWriteBoundary::new("orders"));
        let recorder = outcomes.track(PG, "orders", "restore-run-2", Arc::clone(&after));
        assert!(after.begin_target_writes());
        assert!(recorder.writes_began());
        drop(recorder);
        assert!(matches!(
            outcomes.progress(PG, "orders", "restore-run-2"),
            WorkerRestoreProgress::Ended {
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
            PG,
            "orders",
            "restore-run-1",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        let _second = outcomes.track(
            PG,
            "orders",
            "restore-run-2",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        first.finish(WorkerRestoreProgress::Finished {
            outcome: WorkerRestoreResult::Completed,
        });
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-1"),
            WorkerRestoreProgress::Unknown
        );
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-2"),
            WorkerRestoreProgress::Running {
                writes_began: false
            }
        );
    }

    /// The same service's restores by different engines are separate
    /// entries: a MongoDB restore does not answer for a WAL-G one.
    #[test]
    fn engines_are_tracked_apart() {
        let outcomes = outcomes();
        let _walg = outcomes.track(
            PG,
            "orders",
            "restore-run-1",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        let mongo = outcomes.track(
            WorkerRestoreEngine::Mongodb,
            "orders",
            "restore-run-1",
            Arc::new(TargetWriteBoundary::new("orders")),
        );
        mongo.finish_with(&Ok::<_, String>(WorkerRestoreResult::Completed));
        assert_eq!(
            outcomes.progress(PG, "orders", "restore-run-1"),
            WorkerRestoreProgress::Running {
                writes_began: false
            }
        );
        assert_eq!(
            outcomes.progress(WorkerRestoreEngine::Mongodb, "orders", "restore-run-1"),
            WorkerRestoreProgress::Finished {
                outcome: WorkerRestoreResult::Completed
            }
        );
    }

    /// An unnamed restore is not tracked; an engine that could not run is
    /// recorded as ended with its error.
    #[test]
    fn finish_with_records_the_engine_error() {
        let outcomes = outcomes();
        let writes = Arc::new(TargetWriteBoundary::new("cache"));
        assert!(outcomes
            .track_named(WorkerRestoreEngine::Redis, "cache", None, &writes)
            .is_none());
        let recorder = outcomes
            .track_named(
                WorkerRestoreEngine::Redis,
                "cache",
                Some("restore-run-3"),
                &writes,
            )
            .expect("a named restore is tracked");
        recorder.finish_with(&Err::<WorkerRestoreResult, _>("no such container"));
        assert_eq!(
            outcomes.progress(WorkerRestoreEngine::Redis, "cache", "restore-run-3"),
            WorkerRestoreProgress::Ended {
                writes_began: false,
                reason: "no such container".to_string()
            }
        );
    }
}
