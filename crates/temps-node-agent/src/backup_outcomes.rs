// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The worker backups this node runs (PostgreSQL WAL-G, MongoDB, MariaDB),
//! kept after their requests end.
//!
//! A backup runs in its own task (`run_detached` in
//! `service_data_handlers`), which outlives the request that started it: a
//! control plane whose request timed out or whose connection dropped does
//! not stop it, and dropping a `docker exec` stream would not stop the
//! upload inside the container anyway. So:
//!
//! - A second backup of a service while one runs is refused as busy
//!   ([`BackupInFlight`]), whatever its id: two `backup-push`es against one
//!   database would overlap. The control plane waits for the node, as for
//!   any busy node.
//! - A PostgreSQL WAL-G base backup's outcome is recorded by its task, under
//!   the request's `backup_uuid`, for the control plane to ask for instead
//!   of starting another backup.
//!
//! The running backup of each service, which holds it, is kept apart from
//! the outcomes of ended backups, which are kept by service and backup id:
//! a backup that starts after another ended never hides that one's
//! outcome from its caller. Ended outcomes are dropped after [`RETENTION`],
//! and at most [`ENDED_PER_SERVICE`] are kept per service (the oldest go
//! first), so both maps are bounded by the services on this node. Nothing
//! here is on a request hot path.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use temps_service_protocol::{PostgresWalgBackupProgress, PostgresWalgBackupResult};

/// How long an ended backup's outcome is kept for its caller to read.
pub(crate) const RETENTION: Duration = Duration::from_secs(24 * 3600);

/// How many ended outcomes are kept per service: far more than the backups
/// a caller can still be waiting on, as a service runs one at a time.
pub(crate) const ENDED_PER_SERVICE: usize = 16;

/// The worker backups of this node.
pub(crate) static WORKER_BACKUPS: LazyLock<BackupOutcomes> = LazyLock::new(BackupOutcomes::default);

/// How a backup ended.
#[derive(Debug, Clone)]
enum Outcome {
    /// A WAL-G base backup's engine outcome.
    PostgresWalg(PostgresWalgBackupResult),
    /// Another engine's backup, whose outcome went back on its request.
    Reported,
    /// The engine ended without an outcome, for this reason.
    Ended(String),
}

/// A backup of the service is already running on this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackupInFlight {
    /// The id the running backup was started with.
    pub(crate) backup_uuid: String,
}

#[derive(Default)]
struct State {
    /// Service name → the id of its running backup.
    running: HashMap<String, String>,
    /// (service name, backup id) → when and how that backup ended.
    ended: HashMap<(String, String), Ended>,
    /// Orders ended outcomes, oldest first, as instants can tie.
    next_seq: u64,
}

struct Ended {
    at: Instant,
    seq: u64,
    outcome: Outcome,
}

/// Backups by service name; see the module documentation.
#[derive(Default)]
pub(crate) struct BackupOutcomes {
    state: Mutex<State>,
}

impl BackupOutcomes {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Every mutation is a single insert, remove or retain: a panic while
        // holding the lock leaves valid maps.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record that a backup of `service` started under `backup_uuid`, unless
    /// one of the service is still running. The returned tracker holds the
    /// service until it is finished or dropped (the engine ended, panicked,
    /// or was stopped at its deadline); move it into the backup's task.
    pub(crate) fn begin(
        &'static self,
        service: &str,
        backup_uuid: &str,
    ) -> Result<BackupTracker, BackupInFlight> {
        let mut state = self.lock();
        let now = Instant::now();
        state
            .ended
            .retain(|_, ended| now.saturating_duration_since(ended.at) < RETENTION);
        if let Some(running) = state.running.get(service) {
            return Err(BackupInFlight {
                backup_uuid: running.clone(),
            });
        }
        state
            .running
            .insert(service.to_string(), backup_uuid.to_string());
        Ok(BackupTracker {
            outcomes: self,
            service: service.to_string(),
            backup_uuid: backup_uuid.to_string(),
            ended: false,
        })
    }

    /// Where the WAL-G base backup of `service` started under `backup_uuid`
    /// is. `Unknown` when this node has none under that id.
    pub(crate) fn walg_progress(
        &self,
        service: &str,
        backup_uuid: &str,
    ) -> PostgresWalgBackupProgress {
        let state = self.lock();
        if state
            .running
            .get(service)
            .is_some_and(|running| running == backup_uuid)
        {
            return PostgresWalgBackupProgress::Running;
        }
        match state
            .ended
            .get(&(service.to_string(), backup_uuid.to_string()))
            .map(|ended| &ended.outcome)
        {
            None => PostgresWalgBackupProgress::Unknown,
            Some(Outcome::PostgresWalg(outcome)) => PostgresWalgBackupProgress::Finished {
                outcome: outcome.clone(),
            },
            Some(Outcome::Ended(reason)) => PostgresWalgBackupProgress::Ended {
                reason: reason.clone(),
            },
            Some(Outcome::Reported) => PostgresWalgBackupProgress::Ended {
                reason: format!(
                    "backup {backup_uuid} of service '{service}' was not a WAL-G base backup"
                ),
            },
        }
    }

    fn end(&self, service: &str, backup_uuid: &str, outcome: Outcome) {
        let mut state = self.lock();
        if state
            .running
            .get(service)
            .is_none_or(|running| running != backup_uuid)
        {
            return;
        }
        state.running.remove(service);
        let mut kept: Vec<(u64, String)> = state
            .ended
            .iter()
            .filter(|((name, _), _)| name == service)
            .map(|((_, id), ended)| (ended.seq, id.clone()))
            .collect();
        if kept.len() >= ENDED_PER_SERVICE {
            kept.sort();
            for (_, id) in kept.iter().take(kept.len() + 1 - ENDED_PER_SERVICE) {
                state.ended.remove(&(service.to_string(), id.clone()));
            }
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        state.ended.insert(
            (service.to_string(), backup_uuid.to_string()),
            Ended {
                at: Instant::now(),
                seq,
                outcome,
            },
        );
    }
}

/// A running backup's hold on its service; see [`BackupOutcomes::begin`].
pub(crate) struct BackupTracker {
    outcomes: &'static BackupOutcomes,
    service: String,
    backup_uuid: String,
    ended: bool,
}

impl BackupTracker {
    fn end(&mut self, outcome: Outcome) {
        self.ended = true;
        self.outcomes.end(&self.service, &self.backup_uuid, outcome);
    }

    /// Record a WAL-G base backup's engine result.
    pub(crate) fn finish_walg<E: std::fmt::Display>(
        mut self,
        result: &Result<PostgresWalgBackupResult, E>,
    ) {
        let outcome = match result {
            Ok(outcome) => Outcome::PostgresWalg(outcome.clone()),
            Err(error) => Outcome::Ended(format!("the WAL-G base backup could not run: {error}")),
        };
        self.end(outcome);
    }

    /// Record that another engine's backup ended; its outcome went back on
    /// its request.
    pub(crate) fn finish(mut self) {
        self.end(Outcome::Reported);
    }
}

impl Drop for BackupTracker {
    fn drop(&mut self) {
        if !self.ended {
            self.outcomes.end(
                &self.service,
                &self.backup_uuid,
                Outcome::Ended(
                    "the backup stopped before it reported an outcome (it passed its deadline \
                     or stopped unexpectedly)"
                        .to_string(),
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test its own registry, as the process-wide one is shared.
    fn outcomes() -> &'static BackupOutcomes {
        Box::leak(Box::default())
    }

    fn completed() -> PostgresWalgBackupResult {
        PostgresWalgBackupResult::Completed {
            restarted: false,
            stdout: "Wrote backup with name base_000000010000000000000004".to_string(),
            stderr: String::new(),
            lsn: Some("0/4000100".to_string()),
        }
    }

    /// The live failure: a retry after a dropped connection started a
    /// second `backup-push` of the same database while the first still ran.
    /// Any backup of a service is refused while one runs, whatever its id.
    #[test]
    fn a_second_backup_of_a_running_service_is_refused() {
        let outcomes = outcomes();
        let first = outcomes.begin("orders", "b-1").expect("first");
        assert_eq!(
            outcomes.begin("orders", "b-2").err(),
            Some(BackupInFlight {
                backup_uuid: "b-1".to_string()
            })
        );
        assert_eq!(
            outcomes.begin("orders", "b-1").err(),
            Some(BackupInFlight {
                backup_uuid: "b-1".to_string()
            }),
            "a retry under the same id too"
        );
        assert!(
            outcomes.begin("billing", "b-3").is_ok(),
            "another service runs"
        );

        first.finish();
        assert!(
            outcomes.begin("orders", "b-2").is_ok(),
            "free once it ended"
        );
    }

    #[test]
    fn a_walg_backup_reports_running_then_its_outcome() {
        let outcomes = outcomes();
        let tracker = outcomes.begin("orders", "b-1").expect("begin");
        assert_eq!(
            outcomes.walg_progress("orders", "b-1"),
            PostgresWalgBackupProgress::Running
        );
        tracker.finish_walg::<String>(&Ok(completed()));
        assert_eq!(
            outcomes.walg_progress("orders", "b-1"),
            PostgresWalgBackupProgress::Finished {
                outcome: completed()
            }
        );
    }

    #[test]
    fn another_id_or_service_is_unknown() {
        let outcomes = outcomes();
        let _tracker = outcomes.begin("orders", "b-1").expect("begin");
        assert_eq!(
            outcomes.walg_progress("orders", "b-2"),
            PostgresWalgBackupProgress::Unknown
        );
        assert_eq!(
            outcomes.walg_progress("billing", "b-1"),
            PostgresWalgBackupProgress::Unknown
        );
    }

    /// A backup whose task ended without an outcome (stopped at its
    /// deadline, panicked) frees its service and says so.
    #[test]
    fn a_backup_dropped_without_an_outcome_ends_and_frees_its_service() {
        let outcomes = outcomes();
        drop(outcomes.begin("orders", "b-1").expect("begin"));
        assert!(matches!(
            outcomes.walg_progress("orders", "b-1"),
            PostgresWalgBackupProgress::Ended { reason } if reason.contains("before it reported")
        ));
        assert!(outcomes.begin("orders", "b-2").is_ok());
    }

    #[test]
    fn an_engine_error_is_recorded_as_ended() {
        let outcomes = outcomes();
        let tracker = outcomes.begin("orders", "b-1").expect("begin");
        tracker.finish_walg(&Err("the container is gone"));
        assert!(matches!(
            outcomes.walg_progress("orders", "b-1"),
            PostgresWalgBackupProgress::Ended { reason } if reason.contains("the container is gone")
        ));
    }

    /// The reported failure: backup A's request went unanswered and its
    /// caller asks for its outcome, but backup B of the same service began
    /// after A finished and before that ask. A's outcome must still be read.
    #[test]
    fn a_finished_outcome_survives_the_next_backup_of_its_service() {
        let outcomes = outcomes();
        outcomes
            .begin("orders", "b-1")
            .expect("first")
            .finish_walg::<String>(&Ok(completed()));
        let _second = outcomes.begin("orders", "b-2").expect("second");
        assert_eq!(
            outcomes.walg_progress("orders", "b-1"),
            PostgresWalgBackupProgress::Finished {
                outcome: completed()
            }
        );
        assert_eq!(
            outcomes.walg_progress("orders", "b-2"),
            PostgresWalgBackupProgress::Running
        );
    }

    #[test]
    fn at_most_the_latest_outcomes_of_a_service_are_kept() {
        let outcomes = outcomes();
        for n in 0..=ENDED_PER_SERVICE {
            outcomes
                .begin("orders", &format!("b-{n}"))
                .expect("begin")
                .finish_walg::<String>(&Ok(completed()));
        }
        assert_eq!(
            outcomes.walg_progress("orders", "b-0"),
            PostgresWalgBackupProgress::Unknown,
            "the oldest went"
        );
        for n in 1..=ENDED_PER_SERVICE {
            assert!(matches!(
                outcomes.walg_progress("orders", &format!("b-{n}")),
                PostgresWalgBackupProgress::Finished { .. }
            ));
        }
    }
}
