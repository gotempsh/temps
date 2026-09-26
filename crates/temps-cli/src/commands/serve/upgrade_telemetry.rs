// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Anonymous `upgrade_completed` / `upgrade_failed` telemetry for startup.
//!
//! A boot is an upgrade when the database already has applied migrations and
//! this binary brings new ones. If applying them fails, the process exits
//! before the telemetry plugin exists, so the failure is sent synchronously
//! here with a short, bounded request. On success the event is handed to the
//! console and reported with `instance_started`.
//!
//! Only counts and a fixed failure code are sent; the Temps version is already
//! stamped on every event, and the previous version is visible from the
//! instance's earlier events.

use std::path::Path;

use sea_orm::DatabaseConnection;
use temps_core::telemetry::{TelemetryEvent, TelemetryEventKind};

/// A startup that applies new migrations to an existing database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeProbe {
    pending_migrations: usize,
}

impl UpgradeProbe {
    /// `Some` when this boot upgrades an existing database. `None` for a fresh
    /// install (every migration pending), a restart with nothing pending, or
    /// when the pending list can't be read.
    pub async fn detect(db: &DatabaseConnection) -> Option<Self> {
        let pending = temps_database::get_pending_migration_names(db)
            .await
            .ok()?
            .len();
        Self::from_counts(pending, temps_database::defined_migration_count())
    }

    fn from_counts(pending: usize, defined: usize) -> Option<Self> {
        (pending > 0 && pending < defined).then_some(Self {
            pending_migrations: pending,
        })
    }

    pub fn completed_event(&self) -> TelemetryEvent {
        TelemetryEvent::new(TelemetryEventKind::UpgradeCompleted)
            .with("migrations_applied", self.pending_migrations as u64)
    }

    pub fn failed_event(&self, error_message: &str) -> TelemetryEvent {
        TelemetryEvent::new(TelemetryEventKind::UpgradeFailed)
            .with("stage", "migrations")
            .with("pending_migrations", self.pending_migrations as u64)
            .with_failure_from_message(error_message)
    }
}

/// Apply pending migrations, reporting `upgrade_failed` before returning the
/// error when this boot was an upgrade. Returns the probe on success so the
/// console can report `upgrade_completed` once telemetry is running.
pub async fn run_migrations_reporting_upgrade(
    db: &DatabaseConnection,
    data_dir: &Path,
) -> Result<Option<UpgradeProbe>, temps_core::ServiceError> {
    let probe = UpgradeProbe::detect(db).await;
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

/// Build a one-off reporter with the instance's identity and send `event`,
/// waiting for the request. Any failure (no identity, opt-out, network) is
/// silent: it must never replace the migration error the operator needs.
async fn send_before_exit(db: &DatabaseConnection, data_dir: &Path, event: TelemetryEvent) {
    let Ok(stateless_id) = temps_config::stateless_telemetry_anonymous_id(db).await else {
        return;
    };
    let Ok(reporter) = temps_telemetry::TelemetryService::new_for_installation(
        data_dir,
        env!("TEMPS_VERSION"),
        stateless_id.as_deref(),
    ) else {
        return;
    };
    reporter.send_now(event).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_and_idle_restart_are_not_upgrades() {
        assert_eq!(UpgradeProbe::from_counts(120, 120), None);
        assert_eq!(UpgradeProbe::from_counts(0, 120), None);
    }

    #[test]
    fn pending_migrations_on_existing_database_are_an_upgrade() {
        let probe = UpgradeProbe::from_counts(3, 120).expect("upgrade");
        let completed = probe.completed_event();
        assert_eq!(completed.event_type, "upgrade_completed");
        assert_eq!(completed.properties["migrations_applied"], 3);
    }

    #[test]
    fn failed_upgrade_sends_code_not_message() {
        let probe = UpgradeProbe::from_counts(2, 120).expect("upgrade");
        let event = probe.failed_event(
            "Migration m20260901_add_index failed: canceling statement due to lock timeout on relation \"customer_orders\"",
        );
        assert_eq!(event.event_type, "upgrade_failed");
        assert_eq!(event.properties["stage"], "migrations");
        assert_eq!(event.properties["pending_migrations"], 2);
        assert_eq!(event.properties["failure_code"], "timeout");
        let serialized = serde_json::to_string(&event).unwrap();
        assert!(!serialized.contains("customer_orders"));
    }
}
