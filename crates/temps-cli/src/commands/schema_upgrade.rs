// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Upgrade safety that runs before any schema migration, shared by
//! `temps serve` (automatic migrations on boot) and `temps migrate` (explicit
//! migrations, also used by the console's "Update now").
//!
//! 1. **Schema guard** — refuse a database that a newer release migrated
//!    ([`temps_database::SchemaGuardError::DatabaseNewerThanBinary`]).
//! 2. **Pre-migration backup** — opt-in with `--pre-migration-backup`. When
//!    an existing database has pending migrations, dump it to
//!    `<data dir>/backups/pre-migration/` first; if the dump fails, nothing
//!    is migrated. Without the flag an upgrade migrates straight away and
//!    logs how to enable the backup.
//!
//! Either way, a pending upgrade first removes any `pg_dump` container left
//! behind by an interrupted earlier backup: it still holds a lock on every
//! table and would make the first migration time out waiting for it.
//!
//! A fresh install and a restart with nothing pending skip both, so the only
//! steady-state cost is the guard's two small queries.

use std::path::Path;

use temps_backup::pre_migration::{
    create_pre_migration_backup, pre_migration_backup_dir,
    remove_orphaned_pre_migration_backup_containers, PreMigrationBackup, PreMigrationBackupError,
    PreMigrationBackupRequest,
};
use temps_database::{SchemaGuardError, SchemaStatus};
use tracing::info;

/// The CLI flag that opts in to the backup, as shown in errors and logs.
pub const BACKUP_FLAG: &str = "--pre-migration-backup";

/// Whether the pre-migration backup applies to this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupPolicy {
    /// The operator passed [`BACKUP_FLAG`]: take the backup when an upgrade
    /// is pending and refuse to migrate if it fails.
    Enabled,
    /// The default: migrate without taking a backup.
    Disabled,
}

impl BackupPolicy {
    pub fn from_flag(enabled: bool) -> Self {
        if enabled {
            BackupPolicy::Enabled
        } else {
            BackupPolicy::Disabled
        }
    }
}

/// What to do about the backup for a given schema state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupDecision {
    /// Nothing will be migrated on an existing database.
    NotNeeded,
    /// Pending upgrade, but the backup was not requested.
    NotRequested,
    /// Pending upgrade: take the backup before migrating.
    Take,
}

fn decide(status: &SchemaStatus, policy: BackupPolicy) -> BackupDecision {
    match (status, policy) {
        (SchemaStatus::Fresh | SchemaStatus::UpToDate { .. }, _) => BackupDecision::NotNeeded,
        (SchemaStatus::PendingUpgrade { .. }, BackupPolicy::Enabled) => BackupDecision::Take,
        (SchemaStatus::PendingUpgrade { .. }, BackupPolicy::Disabled) => {
            BackupDecision::NotRequested
        }
    }
}

/// Why startup refused to migrate.
#[derive(Debug, thiserror::Error)]
pub enum SchemaUpgradeError {
    #[error(transparent)]
    Guard(#[from] SchemaGuardError),

    #[error(
        "Refusing to apply {pending} pending database migration(s) because the automatic \
         pre-migration backup failed: {source}. Nothing was changed. Fix the cause and start \
         again, or take your own database backup and start without {flag} (accepted by \
         `temps serve` and `temps migrate`) to migrate without the automatic backup."
    )]
    BackupFailed {
        pending: usize,
        flag: &'static str,
        #[source]
        source: PreMigrationBackupError,
    },
}

/// The checked schema state and the backup taken for it, if any.
#[derive(Debug)]
pub struct PreparedUpgrade {
    pub status: SchemaStatus,
    pub backup: Option<PreMigrationBackup>,
}

/// Run the schema guard and, for an upgrade, the pre-migration backup.
///
/// Must run before any migration is applied. Returns an error — and changes
/// nothing — when the database is newer than this binary or when a required
/// backup fails.
pub async fn prepare_schema_upgrade(
    db: &sea_orm::DatabaseConnection,
    database_url: &str,
    data_dir: &Path,
    policy: BackupPolicy,
) -> Result<PreparedUpgrade, SchemaUpgradeError> {
    let status = temps_database::check_schema_compatibility(db).await?;
    let pending = status.pending().len();
    let backup = match decide(&status, policy) {
        BackupDecision::NotNeeded => None,
        BackupDecision::NotRequested => {
            remove_orphaned_pre_migration_backup_containers(data_dir).await;
            info!(
                pending_migrations = pending,
                "Applying {pending} pending migration(s) without a pre-migration backup. \
                 Start with {BACKUP_FLAG} to dump the database to {} first, so you can roll \
                 back to the previous release.",
                pre_migration_backup_dir(data_dir).display()
            );
            None
        }
        BackupDecision::Take => {
            let applied = match &status {
                SchemaStatus::PendingUpgrade { applied, .. } => *applied,
                SchemaStatus::Fresh | SchemaStatus::UpToDate { .. } => 0,
            };
            let binary_version = crate::commands::upgrade::current_version_tag();
            let previous_version =
                crate::commands::serve::upgrade_telemetry::read_last_started_version(data_dir);
            let backup = create_pre_migration_backup(PreMigrationBackupRequest {
                db,
                database_url,
                data_dir,
                binary_version: &binary_version,
                previous_version: previous_version.as_deref(),
                applied_migrations: applied,
                pending_migrations: status.pending(),
            })
            .await
            .map_err(|source| SchemaUpgradeError::BackupFailed {
                pending,
                flag: BACKUP_FLAG,
                source,
            })?;
            info!(
                path = %backup.dump_path.display(),
                manifest = %backup.manifest_path.display(),
                size_bytes = backup.size_bytes,
                elapsed_ms = backup.elapsed.as_millis() as u64,
                method = %backup.method,
                pruned = backup.pruned.len(),
                pending_migrations = pending,
                "Pre-migration database backup written to {}; restore it to roll back \
                 this upgrade (backups directory: {})",
                backup.dump_path.display(),
                pre_migration_backup_dir(data_dir).display()
            );
            Some(backup)
        }
    };
    Ok(PreparedUpgrade { status, backup })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> SchemaStatus {
        SchemaStatus::PendingUpgrade {
            applied: 10,
            pending: vec!["m_next".to_string()],
        }
    }

    #[test]
    fn fresh_installs_and_plain_restarts_never_back_up() {
        for policy in [BackupPolicy::Enabled, BackupPolicy::Disabled] {
            assert_eq!(
                decide(&SchemaStatus::Fresh, policy),
                BackupDecision::NotNeeded
            );
            assert_eq!(
                decide(&SchemaStatus::UpToDate { applied: 10 }, policy),
                BackupDecision::NotNeeded
            );
        }
    }

    #[test]
    fn pending_upgrades_back_up_only_when_enabled() {
        assert_eq!(
            decide(&pending(), BackupPolicy::Enabled),
            BackupDecision::Take
        );
        assert_eq!(
            decide(&pending(), BackupPolicy::Disabled),
            BackupDecision::NotRequested
        );
    }

    #[test]
    fn the_backup_is_off_unless_the_flag_is_passed() {
        assert_eq!(BackupPolicy::from_flag(false), BackupPolicy::Disabled);
        assert_eq!(BackupPolicy::from_flag(true), BackupPolicy::Enabled);
    }

    #[test]
    fn backup_failure_names_the_escape_hatch_and_the_cause() {
        let message = SchemaUpgradeError::BackupFailed {
            pending: 3,
            flag: BACKUP_FLAG,
            source: PreMigrationBackupError::NoDumpTool {
                server_major: 18,
                host: "no pg_dump on PATH".to_string(),
                docker: "socket missing".to_string(),
            },
        }
        .to_string();
        assert!(
            message.contains("3 pending database migration(s)"),
            "{message}"
        );
        assert!(message.contains("Nothing was changed"), "{message}");
        assert!(message.contains(BACKUP_FLAG), "{message}");
        assert!(message.contains("no pg_dump on PATH"), "{message}");
    }
}
