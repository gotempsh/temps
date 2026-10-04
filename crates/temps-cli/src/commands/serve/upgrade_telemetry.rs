// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Anonymous `upgrade_completed` / `upgrade_failed` telemetry for startup.
//!
//! Upgrades are measured where they take effect: the first `temps serve` of a
//! new version. The version of the last successful start is kept in
//! `<data_dir>/last_started_version`, so an upgrade is seen however its
//! migrations were applied (by this process, by `temps migrate`, or by the
//! self-updater's migrate step) and even when the release has none. A start
//! that applies migrations to an already-populated database is also an
//! upgrade, which covers the first start after this file was introduced and
//! installations without a persistent data directory.
//!
//! If applying migrations fails, the process exits before the telemetry
//! plugin exists, so the failure is sent synchronously here with a short,
//! bounded request. On success the event is handed to the console and
//! reported with `instance_started`.
//!
//! Only Temps release versions, a migration count and a fixed failure code are
//! sent.

use std::path::Path;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use temps_core::telemetry::{TelemetryEvent, TelemetryEventKind};
use tracing::debug;

const LAST_STARTED_VERSION_FILE: &str = "last_started_version";

/// Upper bound on reporting a failed upgrade before the process exits,
/// including the identity lookup against a database that may be unhealthy.
const SEND_BEFORE_EXIT_TIMEOUT: Duration = Duration::from_secs(8);

/// The release part of `TEMPS_VERSION` (`v0.1.0` in `v0.1.0 (abc123) built
/// <time>`): stable across rebuilds of the same release, so rebuilding a
/// binary is not mistaken for an upgrade.
fn current_version() -> &'static str {
    release_version(env!("TEMPS_VERSION"))
}

fn release_version(full: &str) -> &str {
    full.split_whitespace().next().unwrap_or(full)
}

/// A startup that moves an existing installation to this version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeProbe {
    /// Version of the last successful start, when it is known.
    previous_version: Option<String>,
    pending_migrations: usize,
}

impl UpgradeProbe {
    /// `Some` when this start upgrades an existing installation. `None` for a
    /// fresh install, a restart of the same version, or when neither the
    /// previous version nor the pending migrations can be read.
    pub async fn detect(db: &DatabaseConnection, data_dir: &Path) -> Option<Self> {
        let pending = temps_database::get_pending_migration_names(db)
            .await
            .map(|names| names.len())
            .ok();
        Self::from_state(
            read_last_started_version(data_dir),
            current_version(),
            pending,
            temps_database::defined_migration_count(),
        )
    }

    fn from_state(
        previous_version: Option<String>,
        current: &str,
        pending: Option<usize>,
        defined: usize,
    ) -> Option<Self> {
        let pending_migrations = pending.unwrap_or(0);
        let version_changed = previous_version
            .as_deref()
            .is_some_and(|previous| previous != current);
        // Some migrations applied and some pending: an existing database.
        // Every migration pending is a fresh install.
        let migrating_existing_db = pending_migrations > 0 && pending_migrations < defined;
        (version_changed || migrating_existing_db).then_some(Self {
            previous_version,
            pending_migrations,
        })
    }

    fn with_versions(&self, event: TelemetryEvent) -> TelemetryEvent {
        event
            .with(
                "from_version",
                self.previous_version
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            )
            .with("to_version", current_version())
    }

    pub fn completed_event(&self) -> TelemetryEvent {
        self.with_versions(TelemetryEvent::new(TelemetryEventKind::UpgradeCompleted))
            .with("migrations_applied", self.pending_migrations as u64)
    }

    pub fn failed_event(&self, error_message: &str) -> TelemetryEvent {
        self.with_versions(TelemetryEvent::new(TelemetryEventKind::UpgradeFailed))
            .with("stage", "migrations")
            .with("pending_migrations", self.pending_migrations as u64)
            .with_failure_from_message(error_message)
    }
}

/// Apply pending migrations, reporting `upgrade_failed` before returning the
/// error when this start was an upgrade. Returns the probe on success so the
/// console can report `upgrade_completed` once telemetry is running.
pub async fn run_migrations_reporting_upgrade(
    db: &DatabaseConnection,
    data_dir: &Path,
) -> Result<Option<UpgradeProbe>, temps_core::ServiceError> {
    let probe = UpgradeProbe::detect(db, data_dir).await;
    match temps_database::run_migrations(db).await {
        Ok(()) => Ok(probe),
        Err(error) => {
            if let Some(probe) = probe {
                send_before_exit(db, data_dir, probe.failed_event(&error.to_string())).await;
            }
            Err(error)
        }
    }
}

/// Finish a successful start: report `upgrade_completed` when this start was
/// an upgrade, then record this release as the last successful start. Called
/// once the console is listening, so a start that fails later (initial admin
/// setup, a listener that cannot bind) is neither counted as a completed
/// upgrade nor recorded, and its retry is still recognised as the upgrade.
pub fn complete_startup(
    probe: Option<&UpgradeProbe>,
    reporter: Option<&std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>>,
    data_dir: &Path,
) {
    if let (Some(probe), Some(reporter)) = (probe, reporter) {
        if reporter.is_enabled() {
            reporter.report(probe.completed_event());
        }
    }
    record_started_version(data_dir);
}

/// Remember this version as the last successful start, so the next start of a
/// different version is recognised as an upgrade. Best-effort: a read-only or
/// scratch data directory only means the next upgrade is detected from its
/// migrations instead.
pub fn record_started_version(data_dir: &Path) {
    let path = data_dir.join(LAST_STARTED_VERSION_FILE);
    if let Err(e) = std::fs::write(&path, current_version()) {
        debug!(
            "Could not record the started version in {}: {}",
            path.display(),
            e
        );
    }
}

/// The recorded version, or `None` when absent or not a plausible release
/// string. The file is local and writable, so its content is validated before
/// it can reach an event.
pub(crate) fn read_last_started_version(data_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(data_dir.join(LAST_STARTED_VERSION_FILE)).ok()?;
    version_label(raw.trim())
}

fn version_label(raw: &str) -> Option<String> {
    let plausible = !raw.is_empty()
        && raw.len() <= 64
        && raw.starts_with(|c: char| c.is_ascii_digit() || c == 'v')
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    plausible.then(|| raw.to_string())
}

/// Build a one-off reporter with the instance's identity and send `event`,
/// waiting at most [`SEND_BEFORE_EXIT_TIMEOUT`]. Any failure (no identity,
/// opt-out, network, timeout) is silent: it must never replace or noticeably
/// delay the migration error the operator needs.
async fn send_before_exit(db: &DatabaseConnection, data_dir: &Path, event: TelemetryEvent) {
    if cfg!(test) {
        return;
    }
    let send = async {
        let Ok(stateless_id) = temps_config::stateless_telemetry_anonymous_id(db).await else {
            return;
        };
        let Ok(reporter) = temps_telemetry::TelemetryService::new_for_installation(
            data_dir,
            current_version(),
            stateless_id.as_deref(),
        ) else {
            return;
        };
        reporter.send_now(event).await;
    };
    if tokio::time::timeout(SEND_BEFORE_EXIT_TIMEOUT, send)
        .await
        .is_err()
    {
        debug!("Timed out reporting the failed upgrade; exiting without it");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn previous(version: &str) -> Option<String> {
        Some(version.to_string())
    }

    #[test]
    fn fresh_install_and_same_version_restart_are_not_upgrades() {
        // Fresh install: nothing recorded, every migration pending.
        assert_eq!(
            UpgradeProbe::from_state(None, "0.2.0", Some(120), 120),
            None
        );
        // Restart of the same version with nothing pending.
        assert_eq!(
            UpgradeProbe::from_state(previous("0.2.0"), "0.2.0", Some(0), 120),
            None
        );
    }

    #[test]
    fn version_change_is_an_upgrade_even_without_pending_migrations() {
        // Migrations already applied by `temps migrate` or the self-updater,
        // or a release that adds none.
        let probe =
            UpgradeProbe::from_state(previous("0.1.9"), "0.2.0", Some(0), 120).expect("upgrade");
        assert_eq!(probe.previous_version.as_deref(), Some("0.1.9"));
        assert_eq!(probe.pending_migrations, 0);
    }

    #[test]
    fn pending_migrations_on_existing_database_are_an_upgrade() {
        let probe = UpgradeProbe::from_state(None, "0.2.0", Some(3), 120).expect("upgrade");
        let completed = probe.completed_event();
        assert_eq!(completed.event_type, "upgrade_completed");
        assert_eq!(completed.properties["migrations_applied"], 3);
        assert_eq!(completed.properties["from_version"], "unknown");
        assert_eq!(completed.properties["to_version"], current_version());
    }

    #[test]
    fn failed_upgrade_sends_code_not_message() {
        let probe =
            UpgradeProbe::from_state(previous("0.1.9"), "0.2.0", Some(2), 120).expect("upgrade");
        let event = probe.failed_event(
            "Migration m20260901_add_index failed: canceling statement due to lock timeout on relation \"customer_orders\"",
        );
        assert_eq!(event.event_type, "upgrade_failed");
        assert_eq!(event.properties["stage"], "migrations");
        assert_eq!(event.properties["pending_migrations"], 2);
        assert_eq!(event.properties["from_version"], "0.1.9");
        assert_eq!(event.properties["failure_code"], "timeout");
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(!serialized.contains("customer_orders"));
    }

    #[test]
    fn recorded_version_round_trips_and_rejects_free_text() {
        let dir = std::env::temp_dir().join(format!(
            "temps-upgrade-telemetry-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(read_last_started_version(&dir), None);
        record_started_version(&dir);
        assert_eq!(
            read_last_started_version(&dir).as_deref(),
            Some(current_version())
        );

        std::fs::write(dir.join(LAST_STARTED_VERSION_FILE), "my company prod box").unwrap();
        assert_eq!(read_last_started_version(&dir), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[derive(Default)]
    struct RecordingReporter {
        events: std::sync::Mutex<Vec<String>>,
    }

    impl temps_core::telemetry::TelemetryReporter for RecordingReporter {
        fn report(&self, event: TelemetryEvent) {
            self.events.lock().unwrap().push(event.event_type);
        }
        fn is_enabled(&self) -> bool {
            true
        }
    }

    #[test]
    fn upgrade_is_completed_and_recorded_only_by_complete_startup() {
        let dir = std::env::temp_dir().join(format!(
            "temps-upgrade-telemetry-startup-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LAST_STARTED_VERSION_FILE), "0.0.1").unwrap();

        let probe = UpgradeProbe::from_state(
            read_last_started_version(&dir),
            current_version(),
            Some(0),
            120,
        )
        .expect("a different recorded release is an upgrade");
        let recording = std::sync::Arc::new(RecordingReporter::default());
        let reporter: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter> =
            recording.clone();

        // Until the console is listening nothing is reported or recorded, so a
        // start that fails before then is retried as the same upgrade.
        assert!(recording.events.lock().unwrap().is_empty());
        assert_eq!(read_last_started_version(&dir).as_deref(), Some("0.0.1"));

        complete_startup(Some(&probe), Some(&reporter), &dir);
        assert_eq!(
            recording.events.lock().unwrap().as_slice(),
            ["upgrade_completed"]
        );
        assert_eq!(
            read_last_started_version(&dir).as_deref(),
            Some(current_version())
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn release_version_drops_commit_and_build_time() {
        assert_eq!(
            release_version("v0.1.0 (abc1234) built 2026-09-27 10:00:00 UTC"),
            "v0.1.0"
        );
        assert_eq!(
            release_version("v0.1.0-nightly.1-3-gabc1234-abc1234 built 2026-09-27 10:00:00 UTC"),
            "v0.1.0-nightly.1-3-gabc1234-abc1234"
        );
        assert!(version_label(current_version()).is_some());
    }

    #[test]
    fn version_label_accepts_release_strings_only() {
        assert_eq!(version_label("0.1.36").as_deref(), Some("0.1.36"));
        assert_eq!(
            version_label("v0.2.0-rc.1+build.5").as_deref(),
            Some("v0.2.0-rc.1+build.5")
        );
        assert_eq!(version_label(""), None);
        assert_eq!(version_label("prod.example.com"), None);
        assert_eq!(version_label("0.1.0; rm -rf"), None);
    }
}
