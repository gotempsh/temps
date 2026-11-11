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
//! One entry per service: its latest backup. Ended entries are dropped
//! after [`RETENTION`], so the map is bounded by the services on this node.
//! Nothing here is on a request hot path.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use temps_service_protocol::{PostgresWalgBackupProgress, PostgresWalgBackupResult};

/// How long an ended backup's outcome is kept for its caller to read.
pub(crate) const RETENTION: Duration = Duration::from_secs(24 * 3600);

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

struct Entry {
    backup_uuid: String,
    ended: Option<(Instant, Outcome)>,
}

/// A backup of the service is already running on this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackupInFlight {
    /// The id the running backup was started with.
    pub(crate) backup_uuid: String,
}

/// Backups by service name; see the module documentation.
#[derive(Default)]
pub(crate) struct BackupOutcomes {
    entries: Mutex<HashMap<String, Entry>>,
}

impl BackupOutcomes {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        // Every mutation is a single insert or field write: a panic while
        // holding the lock leaves a valid map.
        self.entries
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
        let mut entries = self.lock();
        let now = Instant::now();
        entries.retain(|_, entry| {
            entry
                .ended
                .as_ref()
                .is_none_or(|(at, _)| now.saturating_duration_since(*at) < RETENTION)
        });
        if let Some(running) = entries.get(service).filter(|entry| entry.ended.is_none()) {
            return Err(BackupInFlight {
                backup_uuid: running.backup_uuid.clone(),
            });
        }
        entries.insert(
            service.to_string(),
            Entry {
                backup_uuid: backup_uuid.to_string(),
                ended: None,
            },
        );
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
        let entries = self.lock();
        let Some(entry) = entries
            .get(service)
            .filter(|entry| entry.backup_uuid == backup_uuid)
        else {
            return PostgresWalgBackupProgress::Unknown;
        };
        match &entry.ended {
            None => PostgresWalgBackupProgress::Running,
            Some((_, Outcome::PostgresWalg(outcome))) => PostgresWalgBackupProgress::Finished {
                outcome: outcome.clone(),
            },
            Some((_, Outcome::Ended(reason))) => PostgresWalgBackupProgress::Ended {
                reason: reason.clone(),
            },
            Some((_, Outcome::Reported)) => PostgresWalgBackupProgress::Ended {
                reason: format!(
                    "backup {backup_uuid} of service '{service}' was not a WAL-G base backup"
                ),
            },
        }
    }

    fn end(&self, service: &str, backup_uuid: &str, outcome: Outcome) {
        let mut entries = self.lock();
        if let Some(entry) = entries
            .get_mut(service)
            .filter(|entry| entry.backup_uuid == backup_uuid && entry.ended.is_none())
        {
            entry.ended = Some((Instant::now(), outcome));
        }
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

    #[test]
    fn a_later_backup_replaces_the_entry() {
        let outcomes = outcomes();
        outcomes
            .begin("orders", "b-1")
            .expect("first")
            .finish_walg::<String>(&Ok(completed()));
        let _second = outcomes.begin("orders", "b-2").expect("second");
        assert_eq!(
            outcomes.walg_progress("orders", "b-1"),
            PostgresWalgBackupProgress::Unknown
        );
        assert_eq!(
            outcomes.walg_progress("orders", "b-2"),
            PostgresWalgBackupProgress::Running
        );
    }
}
