// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep Temps' own PostgreSQL from retaining WAL it will never archive.
//!
//! The Compose package starts Temps' database with `archive_mode=on` and no
//! `archive_command`, so that enabling control-plane backups later only needs
//! an `ALTER SYSTEM SET archive_command` and a reload, not a restart (see
//! [`super::walg`]). Until then PostgreSQL treats every WAL segment as waiting
//! to be archived and never removes one, and `archive_timeout` closes a 16 MB
//! segment every minute: an install that never configures backups fills its
//! disk at roughly 23 GB a day.
//!
//! WAL written before any base backup exists cannot be used to restore
//! anything, so until backups are configured the segments are discarded:
//! `archive_command` is set to `/bin/true`, which PostgreSQL treats as a
//! successful archive. Configuring backups replaces it with the WAL-G command,
//! as before. A server whose `archive_command` is already set, or which does
//! not archive at all, is left alone.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use tracing::{info, warn};

/// `archive_command` that marks a segment archived without keeping it.
pub const DISCARD_ARCHIVE_COMMAND: &str = "/bin/true";

/// What [`stop_unarchived_wal_retention`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalRetentionOutcome {
    /// `archive_mode` is off: PostgreSQL recycles WAL normally.
    NotArchiving,
    /// An `archive_command` is configured; it decides what happens to WAL.
    ArchiveCommandSet,
    /// WAL was being retained with nothing to archive it; it is now
    /// discarded until backups are configured.
    NowDiscarding,
}

/// Why the archive settings of Temps' database could not be read or changed.
#[derive(Debug, thiserror::Error)]
pub enum WalRetentionError {
    #[error("Failed to read archive settings of Temps' own database: {0}")]
    Read(#[source] sea_orm::DbErr),

    #[error(
        "Temps' own database has archive_mode={archive_mode} and no archive_command, so it keeps \
         every WAL segment until the disk is full, but the role Temps connects as is not a \
         superuser and cannot change it. As a superuser, run: ALTER SYSTEM SET archive_command \
         = '{DISCARD_ARCHIVE_COMMAND}'; SELECT pg_reload_conf();"
    )]
    NotSuperuser { archive_mode: String },

    #[error(
        "Failed to stop Temps' own database from retaining unarchived WAL (archive_mode \
         {archive_mode}, no archive_command) while running '{statement}': {source}"
    )]
    Change {
        archive_mode: String,
        statement: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Whether a server with these settings retains WAL with nothing to archive
/// it.
pub fn retains_unarchived_wal(archive_mode: &str, archive_command: &str) -> bool {
    matches!(archive_mode.trim(), "on" | "always") && archive_command.trim().is_empty()
}

/// Make Temps' own database discard WAL that nothing will archive (see the
/// module documentation). Safe to call on every start.
pub async fn stop_unarchived_wal_retention<C: ConnectionTrait>(
    db: &C,
) -> Result<WalRetentionOutcome, WalRetentionError> {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT current_setting('archive_mode') AS archive_mode, \
                    current_setting('archive_command') AS archive_command, \
                    (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) AS superuser",
        ))
        .await
        .map_err(WalRetentionError::Read)?
        .ok_or_else(|| {
            WalRetentionError::Read(sea_orm::DbErr::RecordNotFound(
                "archive settings query returned no row".to_string(),
            ))
        })?;
    let archive_mode: String = row
        .try_get("", "archive_mode")
        .map_err(WalRetentionError::Read)?;
    let archive_command: String = row
        .try_get("", "archive_command")
        .map_err(WalRetentionError::Read)?;
    let superuser: Option<bool> = row
        .try_get("", "superuser")
        .map_err(WalRetentionError::Read)?;

    if !matches!(archive_mode.trim(), "on" | "always") {
        return Ok(WalRetentionOutcome::NotArchiving);
    }
    if !retains_unarchived_wal(&archive_mode, &archive_command) {
        return Ok(WalRetentionOutcome::ArchiveCommandSet);
    }
    if superuser != Some(true) {
        return Err(WalRetentionError::NotSuperuser { archive_mode });
    }

    // `ALTER SYSTEM` cannot run inside a transaction block: two statements.
    for statement in [
        "ALTER SYSTEM SET archive_command = '/bin/true'",
        "SELECT pg_reload_conf()",
    ] {
        db.execute_unprepared(statement)
            .await
            .map_err(|source| WalRetentionError::Change {
                archive_mode: archive_mode.clone(),
                statement,
                source,
            })?;
    }
    Ok(WalRetentionOutcome::NowDiscarding)
}

/// [`stop_unarchived_wal_retention`], logging what it found. Never fails the
/// caller: a database it cannot change keeps working, with a warning that
/// says what to run.
pub async fn guard_unarchived_wal<C: ConnectionTrait>(db: &C) {
    match stop_unarchived_wal_retention(db).await {
        Ok(WalRetentionOutcome::NowDiscarding) => warn!(
            "Temps' database had archive_mode on and no archive_command, so it was keeping every \
             WAL segment until the disk filled. Set archive_command to '{}': WAL is discarded \
             until backups are configured, which replaces it with the backup command",
            DISCARD_ARCHIVE_COMMAND
        ),
        Ok(outcome) => info!(?outcome, "Temps' database WAL archiving checked"),
        Err(error) => warn!(%error, "Temps' database may be retaining WAL it never archives"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn settings(mode: &str, command: &str, superuser: bool) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("archive_mode", Value::from(mode.to_string())),
            ("archive_command", Value::from(command.to_string())),
            ("superuser", Value::from(Some(superuser))),
        ])
    }

    #[test]
    fn only_archiving_without_a_command_retains_wal() {
        assert!(retains_unarchived_wal("on", ""));
        assert!(retains_unarchived_wal("always", "  "));
        assert!(!retains_unarchived_wal("off", ""));
        assert!(!retains_unarchived_wal(
            "on",
            ". /var/lib/postgresql/walg.env && wal-g wal-push %p"
        ));
        assert!(!retains_unarchived_wal("on", DISCARD_ARCHIVE_COMMAND));
    }

    #[tokio::test]
    async fn archiving_without_a_command_is_switched_to_discard() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings("on", "", true)]])
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
            ])
            .into_connection();

        let outcome = stop_unarchived_wal_retention(&db).await.unwrap();

        assert_eq!(outcome, WalRetentionOutcome::NowDiscarding);
        let log = db.into_transaction_log();
        let statements: Vec<String> = log.iter().map(|t| format!("{t:?}")).collect();
        assert!(
            statements
                .iter()
                .any(|s| s.contains("ALTER SYSTEM SET archive_command = '/bin/true'")),
            "{statements:?}"
        );
        assert!(
            statements.iter().any(|s| s.contains("pg_reload_conf()")),
            "{statements:?}"
        );
    }

    #[tokio::test]
    async fn a_configured_command_or_no_archiving_is_left_alone() {
        for (mode, command, expected) in [
            (
                "on",
                ". /var/lib/postgresql/walg.env && wal-g wal-push %p",
                WalRetentionOutcome::ArchiveCommandSet,
            ),
            ("off", "", WalRetentionOutcome::NotArchiving),
        ] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![settings(mode, command, true)]])
                .into_connection();

            let outcome = stop_unarchived_wal_retention(&db).await.unwrap();

            assert_eq!(outcome, expected, "{mode} / {command}");
            // Only the settings query ran: nothing was changed.
            assert_eq!(db.into_transaction_log().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_role_that_cannot_change_it_gets_the_statement_to_run() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings("on", "", false)]])
            .into_connection();

        let error = stop_unarchived_wal_retention(&db)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("not a superuser"), "{error}");
        assert!(
            error.contains("ALTER SYSTEM SET archive_command = '/bin/true'"),
            "{error}"
        );
    }
}
