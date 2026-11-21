// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Keep the bundled control-plane database from retaining WAL while no
//! archive destination exists.
//!
//! The bundled Compose files start PostgreSQL with `archive_mode=on`, because
//! `archive_mode` only changes on a restart and Temps cannot restart the
//! database it runs on when a backup is first configured. `archive_command`
//! is left to Temps, which sets it to `wal-g wal-push` before the first
//! internal backup. Until then PostgreSQL has archiving enabled with nothing
//! to archive to, and in that state it keeps every completed WAL segment in
//! `pg_wal` and waits for a command. An install that never configures backups
//! fills its disk this way.
//!
//! On startup Temps gives such a database a command that accepts each segment
//! without storing it. PostgreSQL then recycles WAL as it would with archiving
//! off, and clears any backlog already built up. WAL written before the first
//! base backup is not needed to restore it, so nothing restorable is lost. The
//! first internal backup replaces the command with a real one before it starts.

use super::*;
use sea_orm::ConnectionTrait;

/// `archive_command` for a bundled database with no archive destination yet:
/// it reports every segment archived and keeps none of them.
pub const NO_ARCHIVE_DESTINATION_COMMAND: &str = "/bin/true";

/// The WAL archiving settings the control-plane database is running with.
#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub(super) struct ArchiveSettings {
    pub archive_mode: String,
    pub archive_command: String,
    /// `None` on PostgreSQL releases before 15, which have no
    /// `archive_library`.
    pub archive_library: Option<String>,
}

impl ArchiveSettings {
    /// Whether PostgreSQL is keeping every completed WAL segment because
    /// archiving is on and neither a command nor a library receives them.
    pub(super) fn retains_wal_without_destination(&self) -> bool {
        let archiving = matches!(
            self.archive_mode.trim().to_ascii_lowercase().as_str(),
            "on" | "always"
        );
        let no_command = self.archive_command.trim().is_empty();
        let no_library = self
            .archive_library
            .as_deref()
            .is_none_or(|library| library.trim().is_empty());
        archiving && no_command && no_library
    }
}

/// What the startup check did to the control-plane database's archiving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalArchiveReconcile {
    /// Archiving is off, or something already receives the WAL.
    Unaffected,
    /// The bundled database was retaining WAL with no destination; it now
    /// recycles it normally until an internal backup sets up archiving.
    Released { container_id: String },
    /// The database retains WAL with no destination, but it is not a
    /// container Temps runs, so its settings belong to its operator.
    LeftToOperator { reason: String },
}

impl BackupService {
    /// Stop the bundled control-plane database from retaining WAL while no
    /// archive destination is configured.
    ///
    /// A database that is not the local container Temps backs up with WAL-G
    /// (managed PostgreSQL, a database on another host) is never changed: its
    /// archiving settings are its operator's, and an empty `archive_command`
    /// there may be a deliberate pause.
    pub async fn reconcile_internal_wal_archiving(
        &self,
    ) -> Result<InternalArchiveReconcile, BackupError> {
        self.reconcile_internal_wal_archiving_with(|| async {
            self.find_internal_db_container()
                .await
                .map(|(container_id, _pgdata)| container_id)
        })
        .await
    }

    /// [`Self::reconcile_internal_wal_archiving`] with the bundled-container
    /// lookup supplied, so the decision can be tested without Docker.
    pub(super) async fn reconcile_internal_wal_archiving_with<F, Fut>(
        &self,
        locate_bundled_container: F,
    ) -> Result<InternalArchiveReconcile, BackupError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, BackupError>>,
    {
        let settings = self.read_archive_settings().await?;
        if !settings.retains_wal_without_destination() {
            return Ok(InternalArchiveReconcile::Unaffected);
        }

        let container_id = match locate_bundled_container().await {
            Ok(container_id) => container_id,
            Err(error) => {
                return Ok(InternalArchiveReconcile::LeftToOperator {
                    reason: error.to_string(),
                })
            }
        };

        // ALTER SYSTEM cannot run in a transaction block, so each statement
        // runs on its own.
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            NO_ARCHIVE_DESTINATION_COMMAND.replace('\'', "''")
        );
        self.db
            .execute_unprepared(&alter_sql)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed to set archive_command to '{}' on the control-plane database \
                     in container '{}': {}",
                    NO_ARCHIVE_DESTINATION_COMMAND, container_id, e
                ),
            })?;
        self.db
            .execute_unprepared("SELECT pg_reload_conf()")
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed to reload the configuration of the control-plane database \
                     in container '{}' after setting archive_command: {}",
                    container_id, e
                ),
            })?;

        Ok(InternalArchiveReconcile::Released { container_id })
    }

    async fn read_archive_settings(&self) -> Result<ArchiveSettings, BackupError> {
        ArchiveSettings::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT current_setting('archive_mode') AS archive_mode, \
             current_setting('archive_command') AS archive_command, \
             current_setting('archive_library', true) AS archive_library"
                .to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the WAL archiving settings of the control-plane database: {}",
                e
            ),
        })?
        .ok_or_else(|| BackupError::Internal {
            message: "Reading the WAL archiving settings of the control-plane database \
                      returned no row"
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_scope_test_service;
    use super::*;
    use sea_orm::{MockDatabase, MockExecResult};

    fn settings(mode: &str, command: &str, library: Option<&str>) -> ArchiveSettings {
        ArchiveSettings {
            archive_mode: mode.to_string(),
            archive_command: command.to_string(),
            archive_library: library.map(str::to_string),
        }
    }

    fn settings_row(
        mode: &str,
        command: &str,
        library: Option<&str>,
    ) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("archive_mode", Value::from(mode)),
            ("archive_command", Value::from(command)),
            (
                "archive_library",
                Value::String(library.map(|l| Box::new(l.to_string()))),
            ),
        ])
    }

    fn exec_ok() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }
    }

    #[test]
    fn archiving_on_with_no_command_or_library_retains_wal() {
        assert!(settings("on", "", Some("")).retains_wal_without_destination());
        assert!(settings("always", "  ", None).retains_wal_without_destination());
        assert!(settings("ON", "", None).retains_wal_without_destination());
    }

    #[test]
    fn archiving_off_or_with_a_destination_does_not_retain_wal() {
        assert!(!settings("off", "", Some("")).retains_wal_without_destination());
        assert!(
            !settings("on", ". /x/walg.env && wal-g wal-push %p", Some(""))
                .retains_wal_without_destination()
        );
        assert!(!settings("on", "", Some("basic_archive")).retains_wal_without_destination());
        assert!(
            !settings("on", NO_ARCHIVE_DESTINATION_COMMAND, None).retains_wal_without_destination()
        );
    }

    #[tokio::test]
    async fn bundled_database_retaining_wal_gets_a_discarding_archive_command() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row("on", "", Some(""))]])
            .append_exec_results([exec_ok(), exec_ok()])
            .into_connection();
        let db = Arc::new(db);
        let service = make_scope_test_service(db.clone());

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::Released {
                container_id: "temps-postgres".to_string()
            }
        );
        drop(service);
        let db = Arc::try_unwrap(db).expect("service released its database handle");
        let sql: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|txn| txn.statements().iter())
            .map(|statement| statement.sql.clone())
            .collect();
        assert_eq!(sql.len(), 3, "unexpected statements: {sql:?}");
        assert_eq!(sql[1], "ALTER SYSTEM SET archive_command = '/bin/true'");
        assert_eq!(sql[2], "SELECT pg_reload_conf()");
    }

    /// The reported failure, against a real PostgreSQL started the way the
    /// bundled Compose files start it: completed WAL segments wait in
    /// `pg_wal/archive_status` as `.ready` and are never archived, until the
    /// reconcile gives the archiver a command and it clears them.
    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn bundled_database_stops_retaining_wal_after_the_reconcile() {
        use sea_orm::{ConnectionTrait, Database};
        use testcontainers::core::{ContainerPort, WaitFor};
        use testcontainers::runners::AsyncRunner;
        use testcontainers::{GenericImage, ImageExt};

        let container = match GenericImage::new("pgvector/pgvector", "pg18")
            .with_exposed_port(ContainerPort::Tcp(5432))
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_cmd([
                "postgres",
                "-c",
                "wal_level=replica",
                "-c",
                "archive_mode=on",
                "-c",
                "archive_timeout=60",
            ])
            .start()
            .await
        {
            Ok(container) => container,
            Err(e) => {
                println!("Docker not available, skipping: {e}");
                return;
            }
        };
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        // The ready message is also logged by the init-time server, which
        // then restarts; retry until the final server accepts connections.
        let mut connected = None;
        for _ in 0..40 {
            match Database::connect(&url).await {
                Ok(db) if db.execute_unprepared("SELECT 1").await.is_ok() => {
                    connected = Some(db);
                    break;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            }
        }
        let db = Arc::new(connected.expect("PostgreSQL never accepted connections"));

        async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
            #[derive(FromQueryResult)]
            struct Row {
                n: i64,
            }
            Row::find_by_statement(Statement::from_string(
                DatabaseBackend::Postgres,
                sql.to_string(),
            ))
            .one(db)
            .await
            .unwrap()
            .unwrap()
            .n
        }
        const READY: &str = "SELECT count(*) AS n FROM pg_ls_dir('pg_wal/archive_status') \
                             AS f WHERE f LIKE '%.ready'";
        const ARCHIVED: &str = "SELECT archived_count AS n FROM pg_stat_archiver";

        // Complete a few segments, as normal background activity does.
        db.execute_unprepared("CREATE TABLE wal_filler (x int)")
            .await
            .unwrap();
        for _ in 0..3 {
            db.execute_unprepared("INSERT INTO wal_filler SELECT generate_series(1, 1000)")
                .await
                .unwrap();
            db.execute_unprepared("SELECT pg_switch_wal()")
                .await
                .unwrap();
        }
        let pending = scalar(&db, READY).await;
        assert!(pending >= 3, "expected retained segments, found {pending}");
        assert_eq!(scalar(&db, ARCHIVED).await, 0);

        let service = make_scope_test_service(db.clone());
        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap();
        assert!(matches!(outcome, InternalArchiveReconcile::Released { .. }));

        // The archiver works through the backlog once it has a command.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while scalar(&db, READY).await > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "segments still waiting for the archiver after 30s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert!(scalar(&db, ARCHIVED).await >= pending);

        // A second start leaves the database as it is.
        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("a released database must not trigger container discovery")
            })
            .await
            .unwrap();
        assert_eq!(outcome, InternalArchiveReconcile::Unaffected);
    }

    #[tokio::test]
    async fn database_already_archiving_is_left_alone_without_looking_for_a_container() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row(
                "on",
                ". /home/postgres/pgdata/walg.env && wal-g wal-push %p",
                Some(""),
            )]])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("an archiving database must not trigger container discovery")
            })
            .await
            .unwrap();

        assert_eq!(outcome, InternalArchiveReconcile::Unaffected);
    }

    #[tokio::test]
    async fn database_temps_does_not_run_is_left_to_its_operator() {
        // Only the settings query is queued: an ALTER SYSTEM would fail the
        // mock with "no exec result".
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row("on", "", None)]])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                Err(BackupError::Internal {
                    message: "No Docker container found for database host 'db.example.internal'"
                        .to_string(),
                })
            })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::LeftToOperator {
                reason: "Internal error: No Docker container found for database host \
                         'db.example.internal'"
                    .to_string()
            }
        );
    }

    #[tokio::test]
    async fn failing_alter_system_names_the_container_and_the_command() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row("on", "", Some(""))]])
            .append_exec_errors([sea_orm::DbErr::Custom(
                "permission denied to set parameter \"archive_command\"".to_string(),
            )])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let error = service
            .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("temps-postgres"), "{error}");
        assert!(error.contains("/bin/true"), "{error}");
        assert!(error.contains("permission denied"), "{error}");
    }
}
