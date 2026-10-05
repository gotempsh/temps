// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Upgrade safety that runs before any schema migration, shared by
//! `temps serve` (automatic migrations on boot) and `temps migrate` (explicit
//! migrations, also used by the console's "Update now").
//!
//! 1. **Schema guard** — refuse a database that a newer release migrated
//!    ([`temps_database::SchemaGuardError::DatabaseNewerThanBinary`]).
//! 2. **Pre-migration backup** — when an existing database has pending
//!    migrations, dump it to `<data dir>/backups/pre-migration/` first. If the
//!    dump fails, nothing is migrated unless the operator passed
//!    `--skip-pre-migration-backup` for this one run.
//!
//! A fresh install and a restart with nothing pending skip the backup, so
//! the only steady-state cost is the guard's two small queries.

use std::path::Path;

use temps_backup::pre_migration::{
    create_pre_migration_backup, pre_migration_backup_dir, PreMigrationBackup,
    PreMigrationBackupError, PreMigrationBackupRequest,
};
use temps_database::{SchemaGuardError, SchemaStatus};
use tracing::{info, warn};

/// The CLI flag that opts out of the backup, as shown in errors and logs.
pub const SKIP_BACKUP_FLAG: &str = "--skip-pre-migration-backup";

/// Whether the pre-migration backup applies to this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupPolicy {
    /// Take the backup when an upgrade is pending; refuse to migrate if it fails.
    Required,
    /// The operator passed [`SKIP_BACKUP_FLAG`] for this run.
    SkippedByOperator,
}

impl BackupPolicy {
    pub fn from_skip_flag(skip: bool) -> Self {
        if skip {
            BackupPolicy::SkippedByOperator
        } else {
            BackupPolicy::Required
        }
    }
}

/// What to do about the backup for a given schema state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupDecision {
    /// Nothing will be migrated on an existing database.
    NotNeeded,
    /// Pending upgrade, but the policy waives the backup.
    Waived,
    /// Pending upgrade: take the backup before migrating.
    Take,
}

fn decide(status: &SchemaStatus, policy: BackupPolicy) -> BackupDecision {
    match (status, policy) {
        (SchemaStatus::Fresh | SchemaStatus::UpToDate { .. }, _) => BackupDecision::NotNeeded,
        (SchemaStatus::PendingUpgrade { .. }, BackupPolicy::Required) => BackupDecision::Take,
        (SchemaStatus::PendingUpgrade { .. }, BackupPolicy::SkippedByOperator) => {
            BackupDecision::Waived
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
         again, or take your own database backup and run once with {flag} (accepted by \
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
        BackupDecision::Waived => {
            match policy {
                BackupPolicy::SkippedByOperator => warn!(
                    pending_migrations = pending,
                    "Applying {pending} pending migration(s) WITHOUT the automatic \
                     pre-migration backup ({SKIP_BACKUP_FLAG} was passed). Rolling back \
                     to the previous release will need a backup you took yourself."
                ),
                BackupPolicy::Required => unreachable!("required backups cannot be waived"),
            }
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
                flag: SKIP_BACKUP_FLAG,
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
        for policy in [BackupPolicy::Required, BackupPolicy::SkippedByOperator] {
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
    fn pending_upgrades_back_up_unless_waived() {
        assert_eq!(
            decide(&pending(), BackupPolicy::Required),
            BackupDecision::Take
        );
        assert_eq!(
            decide(&pending(), BackupPolicy::SkippedByOperator),
            BackupDecision::Waived
        );
    }

    #[test]
    fn skip_flag_maps_to_policy() {
        assert_eq!(BackupPolicy::from_skip_flag(false), BackupPolicy::Required);
        assert_eq!(
            BackupPolicy::from_skip_flag(true),
            BackupPolicy::SkippedByOperator
        );
    }

    #[test]
    fn backup_failure_names_the_escape_hatch_and_the_cause() {
        let message = SchemaUpgradeError::BackupFailed {
            pending: 3,
            flag: SKIP_BACKUP_FLAG,
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
        assert!(message.contains(SKIP_BACKUP_FLAG), "{message}");
        assert!(message.contains("no pg_dump on PATH"), "{message}");
    }
}
