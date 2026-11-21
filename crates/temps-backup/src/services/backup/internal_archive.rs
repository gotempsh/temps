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
//! first internal backup replaces the command with a real one before it starts,
//! and puts back what was there if its base backup then fails.
//!
//! Only a command nobody ever set is replaced. PostgreSQL's documented way to
//! pause archiving is to set `archive_command` to an empty string, and a
//! database where someone did that is left alone.

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
    /// Where the running `archive_command` came from (`pg_settings.source`):
    /// `default` when nothing ever set it.
    pub archive_command_source: Option<String>,
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

    /// Whether `archive_command` is still PostgreSQL's built-in empty
    /// default, rather than a value someone set, possibly to pause archiving.
    pub(super) fn archive_command_never_set(&self) -> bool {
        self.archive_command_source.as_deref() == Some("default")
    }

    /// The `archive_command` to put back after enabling `archiving_command`
    /// for a base backup that then failed, or `None` when that was already
    /// the command and there is nothing to undo.
    ///
    /// A command nobody had set goes back to the placeholder rather than
    /// empty, so the failed backup does not leave the database retaining WAL.
    pub(super) fn command_to_restore(&self, archiving_command: &str) -> Option<String> {
        if self.archive_command == archiving_command {
            None
        } else if self.archive_command.trim().is_empty() && self.archive_command_never_set() {
            Some(NO_ARCHIVE_DESTINATION_COMMAND.to_string())
        } else {
            Some(self.archive_command.clone())
        }
    }
}

/// The cluster's identifier, the same for every database and connection on
/// one PostgreSQL server and different between servers.
const SYSTEM_IDENTIFIER_SQL: &str =
    "SELECT system_identifier::text AS system_identifier FROM pg_control_system()";

#[derive(Debug, FromQueryResult)]
struct SystemIdentifier {
    system_identifier: String,
}

/// `Ok` when both reads name the same, non-empty cluster identifier.
fn same_cluster(
    container_id: &str,
    connected: &str,
    in_container: &str,
) -> Result<(), BackupError> {
    let (connected, in_container) = (connected.trim(), in_container.trim());
    if !connected.is_empty() && connected == in_container {
        return Ok(());
    }
    Err(BackupError::Internal {
        message: format!(
            "The control-plane database Temps is connected to (system identifier '{}') is not \
             the one in container '{}' (system identifier '{}')",
            connected, container_id, in_container
        ),
    })
}

/// What the startup check did to the control-plane database's archiving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternalArchiveReconcile {
    /// Archiving is off, or something already receives the WAL.
    Unaffected,
    /// The bundled database was retaining WAL with no destination; it now
    /// recycles it normally until an internal backup sets up archiving.
    Released { container_id: String },
    /// The database retains WAL with no destination, but Temps cannot tell
    /// the setting is its own: `archive_command` was set to empty on purpose,
    /// or the database is not a container Temps runs.
    LeftToOperator { reason: String },
}

impl BackupService {
    /// Stop the bundled control-plane database from retaining WAL while no
    /// archive destination is configured.
    ///
    /// Only an `archive_command` nobody ever set, on the local container Temps
    /// backs up with WAL-G, is changed. An empty value someone set is a
    /// deliberate pause, and a database elsewhere (managed PostgreSQL, another
    /// host) has archiving settings that belong to its operator.
    pub async fn reconcile_internal_wal_archiving(
        &self,
    ) -> Result<InternalArchiveReconcile, BackupError> {
        self.reconcile_internal_wal_archiving_with(|| async {
            let (container_id, _pgdata) = self.find_internal_db_container().await?;
            let docker = bollard::Docker::connect_with_local_defaults().map_err(|e| {
                BackupError::Internal {
                    message: format!("Failed to connect to Docker: {}", e),
                }
            })?;
            self.verify_container_is_connected_database(&docker, &container_id)
                .await?;
            Ok(container_id)
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

        if !settings.archive_command_never_set() {
            return Ok(InternalArchiveReconcile::LeftToOperator {
                reason: format!(
                    "archive_command was set to an empty value (setting source: {}), which \
                     pauses archiving on purpose",
                    settings
                        .archive_command_source
                        .as_deref()
                        .unwrap_or("unknown")
                ),
            });
        }

        let container_id = match locate_bundled_container().await {
            Ok(container_id) => container_id,
            Err(error) => {
                return Ok(InternalArchiveReconcile::LeftToOperator {
                    reason: error.to_string(),
                })
            }
        };

        self.set_archive_command(&container_id, NO_ARCHIVE_DESTINATION_COMMAND)
            .await?;
        Ok(InternalArchiveReconcile::Released { container_id })
    }

    /// Check that `container_id` runs the same PostgreSQL cluster Temps is
    /// connected to, by comparing the cluster's system identifier read over
    /// that connection with the one read inside the container.
    ///
    /// A container whose name, alias or address matches the DATABASE_URL host
    /// is not proof by itself: the host may resolve to another server. Every
    /// change Temps makes to archiving goes through its own connection, so it
    /// must be the container's database.
    pub(super) async fn verify_container_is_connected_database(
        &self,
        docker: &bollard::Docker,
        container_id: &str,
    ) -> Result<(), BackupError> {
        let connected = SystemIdentifier::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            SYSTEM_IDENTIFIER_SQL.to_string(),
        ))
        .one(self.db.as_ref())
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the system identifier of the control-plane database to match \
                 it with container '{}': {}",
                container_id, e
            ),
        })?
        .map(|row| row.system_identifier)
        .unwrap_or_default();

        let database_url = self.config_service.get_database_url();
        let url = url::Url::parse(&database_url).map_err(|e| BackupError::Internal {
            message: format!("Invalid DATABASE_URL: {}", e),
        })?;
        let database = url.path().trim_start_matches('/');
        let mut cmd = vec![
            "psql".to_string(),
            "-U".to_string(),
            url.username().to_string(),
            "-tAc".to_string(),
            SYSTEM_IDENTIFIER_SQL.to_string(),
        ];
        if !database.is_empty() {
            cmd.extend(["-d".to_string(), database.to_string()]);
        }
        let password_env = format!("PGPASSWORD={}", url.password().unwrap_or(""));
        let in_container = temps_providers::externalsvc::exec_util::run_exec(
            docker,
            container_id,
            cmd,
            Some(vec![password_env]),
            std::time::Duration::from_secs(30),
        )
        .await
        .map_err(|e| BackupError::Internal {
            message: format!(
                "Failed to read the system identifier of the database in container '{}': {}",
                container_id, e
            ),
        })?
        .output;

        same_cluster(container_id, &connected, &in_container)
    }

    /// Run a base backup `push` that needs `archiving_command` in place, and
    /// put back the command that was there before (`before`) if it fails.
    ///
    /// Without this, a push that cannot reach the archive leaves
    /// archive_command failing on every segment, and PostgreSQL keeps all WAL
    /// until the disk fills. The push's error is returned either way; a failed
    /// rollback is logged with the command it could not restore.
    pub(super) async fn with_archive_rollback<T, Fut>(
        &self,
        before: &ArchiveSettings,
        archiving_command: &str,
        container_id: &str,
        push: Fut,
    ) -> Result<T, BackupError>
    where
        Fut: std::future::Future<Output = Result<T, BackupError>>,
    {
        let push_error = match push.await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if let Some(command) = before.command_to_restore(archiving_command) {
            match self.set_archive_command(container_id, &command).await {
                Ok(()) => warn!(
                    container_id = %container_id,
                    archive_command = %command,
                    "WAL-G base backup failed after continuous archiving was enabled; \
                     restored the previous archive_command so WAL does not accumulate: {}",
                    push_error
                ),
                Err(rollback_error) => error!(
                    container_id = %container_id,
                    archive_command = %command,
                    "WAL-G base backup failed after continuous archiving was enabled, and \
                     restoring the previous archive_command failed too, so the database may \
                     retain WAL until archiving works: {}. Backup error: {}",
                    rollback_error,
                    push_error
                ),
            }
        }
        Err(push_error)
    }

    /// Set the control-plane database's `archive_command` and reload.
    async fn set_archive_command(
        &self,
        container_id: &str,
        command: &str,
    ) -> Result<(), BackupError> {
        // ALTER SYSTEM cannot run in a transaction block, so each statement
        // runs on its own.
        let alter_sql = format!(
            "ALTER SYSTEM SET archive_command = '{}'",
            command.replace('\'', "''")
        );
        self.db
            .execute_unprepared(&alter_sql)
            .await
            .map_err(|e| BackupError::Internal {
                message: format!(
                    "Failed to set archive_command to '{}' on the control-plane database \
                     in container '{}': {}",
                    command, container_id, e
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
        Ok(())
    }

    pub(super) async fn read_archive_settings(&self) -> Result<ArchiveSettings, BackupError> {
        ArchiveSettings::find_by_statement(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT current_setting('archive_mode') AS archive_mode, \
             current_setting('archive_command') AS archive_command, \
             current_setting('archive_library', true) AS archive_library, \
             (SELECT source FROM pg_settings WHERE name = 'archive_command') \
             AS archive_command_source"
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

    const WALG_COMMAND: &str = ". /home/postgres/pgdata/walg.env && wal-g wal-push %p";

    /// Settings whose `archive_command` nobody set.
    fn settings(mode: &str, command: &str, library: Option<&str>) -> ArchiveSettings {
        settings_from(mode, command, library, "default")
    }

    fn settings_from(
        mode: &str,
        command: &str,
        library: Option<&str>,
        source: &str,
    ) -> ArchiveSettings {
        ArchiveSettings {
            archive_mode: mode.to_string(),
            archive_command: command.to_string(),
            archive_library: library.map(str::to_string),
            archive_command_source: Some(source.to_string()),
        }
    }

    /// A settings query row whose `archive_command` nobody set.
    fn settings_row(
        mode: &str,
        command: &str,
        library: Option<&str>,
    ) -> BTreeMap<&'static str, Value> {
        settings_row_from(mode, command, library, "default")
    }

    fn settings_row_from(
        mode: &str,
        command: &str,
        library: Option<&str>,
        source: &str,
    ) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("archive_mode", Value::from(mode)),
            ("archive_command", Value::from(command)),
            (
                "archive_library",
                Value::String(library.map(|l| Box::new(l.to_string()))),
            ),
            ("archive_command_source", Value::from(source)),
        ])
    }

    /// The SQL a service ran on `db`, once the service has been dropped.
    fn logged_sql(db: Arc<DatabaseConnection>) -> Vec<String> {
        Arc::try_unwrap(db)
            .expect("service released its database handle")
            .into_transaction_log()
            .iter()
            .flat_map(|txn| txn.statements().iter())
            .map(|statement| statement.sql.clone())
            .collect()
    }

    fn push_failure() -> BackupError {
        BackupError::Internal {
            message: "wal-g backup-push failed with exit code 1 in container temps-postgres"
                .to_string(),
        }
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
        let sql = logged_sql(db);
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

        // Emptying the command on purpose pauses archiving, and is left alone.
        db.execute_unprepared("ALTER SYSTEM SET archive_command = ''")
            .await
            .unwrap();
        db.execute_unprepared("SELECT pg_reload_conf()")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("a paused database must not trigger container discovery")
            })
            .await
            .unwrap();
        assert!(
            matches!(outcome, InternalArchiveReconcile::LeftToOperator { .. }),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn database_already_archiving_is_left_alone_without_looking_for_a_container() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row_from(
                "on",
                WALG_COMMAND,
                Some(""),
                "configuration file",
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

    #[test]
    fn a_container_is_the_connected_database_only_with_the_same_system_identifier() {
        assert!(same_cluster(
            "temps-postgres",
            "7301234567890123456",
            "7301234567890123456\n"
        )
        .is_ok());

        let error = same_cluster(
            "temps-postgres",
            "7301234567890123456",
            "7309999999999999999",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("temps-postgres"), "{error}");
        assert!(error.contains("7309999999999999999"), "{error}");

        // An empty read (psql printed nothing) never counts as a match.
        assert!(same_cluster("temps-postgres", "", "").is_err());
    }

    #[tokio::test]
    async fn archiving_paused_on_purpose_is_left_to_its_operator() {
        // Only the settings query is queued: an ALTER SYSTEM would fail the
        // mock with "no exec result".
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![settings_row_from(
                "on",
                "",
                Some(""),
                "configuration file",
            )]])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));

        let outcome = service
            .reconcile_internal_wal_archiving_with(|| async {
                panic!("a paused database must not trigger container discovery")
            })
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InternalArchiveReconcile::LeftToOperator {
                reason: "archive_command was set to an empty value (setting source: \
                         configuration file), which pauses archiving on purpose"
                    .to_string()
            }
        );
    }

    #[test]
    fn a_failed_base_backup_restores_what_archive_command_was() {
        // Nobody had set it: back to the placeholder, not to retaining WAL.
        assert_eq!(
            settings("on", "", Some("")).command_to_restore(WALG_COMMAND),
            Some(NO_ARCHIVE_DESTINATION_COMMAND.to_string())
        );
        // The startup check had released it.
        assert_eq!(
            settings_from(
                "on",
                NO_ARCHIVE_DESTINATION_COMMAND,
                None,
                "configuration file"
            )
            .command_to_restore(WALG_COMMAND),
            Some(NO_ARCHIVE_DESTINATION_COMMAND.to_string())
        );
        // An operator's pause, or their own command, is theirs.
        assert_eq!(
            settings_from("on", "", None, "configuration file").command_to_restore(WALG_COMMAND),
            Some(String::new())
        );
        assert_eq!(
            settings_from("on", "cp %p /archive/%f", None, "configuration file")
                .command_to_restore(WALG_COMMAND),
            Some("cp %p /archive/%f".to_string())
        );
        // Archiving with WAL-G was already in place: nothing to undo.
        assert_eq!(
            settings_from("on", WALG_COMMAND, None, "configuration file")
                .command_to_restore(WALG_COMMAND),
            None
        );
    }

    #[tokio::test]
    async fn failed_first_base_backup_puts_the_placeholder_back() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec_ok(), exec_ok()])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());
        let before = settings_from(
            "on",
            NO_ARCHIVE_DESTINATION_COMMAND,
            None,
            "configuration file",
        );

        let error = service
            .with_archive_rollback(&before, WALG_COMMAND, "temps-postgres", async {
                Err::<(), _>(push_failure())
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("wal-g backup-push failed"), "{error}");
        drop(service);
        assert_eq!(
            logged_sql(db),
            [
                "ALTER SYSTEM SET archive_command = '/bin/true'",
                "SELECT pg_reload_conf()"
            ]
        );
    }

    #[tokio::test]
    async fn failed_base_backup_restores_an_operator_command_with_its_quotes() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec_ok(), exec_ok()])
                .into_connection(),
        );
        let service = make_scope_test_service(db.clone());
        let before = settings_from("on", "cp %p '/archive/%f'", None, "configuration file");

        let result = service
            .with_archive_rollback(&before, WALG_COMMAND, "temps-postgres", async {
                Err::<(), _>(push_failure())
            })
            .await;

        assert!(result.is_err());
        drop(service);
        assert_eq!(
            logged_sql(db)[0],
            "ALTER SYSTEM SET archive_command = 'cp %p ''/archive/%f'''"
        );
    }

    #[tokio::test]
    async fn failed_base_backup_keeps_walg_archiving_that_was_already_in_place() {
        // No exec results are queued: any ALTER SYSTEM would fail the mock.
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = make_scope_test_service(db.clone());
        let before = settings_from("on", WALG_COMMAND, None, "configuration file");

        let result = service
            .with_archive_rollback(&before, WALG_COMMAND, "temps-postgres", async {
                Err::<(), _>(push_failure())
            })
            .await;

        assert!(result.is_err());
        drop(service);
        assert!(logged_sql(db).is_empty());
    }

    #[tokio::test]
    async fn successful_base_backup_keeps_walg_archiving() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = make_scope_test_service(db.clone());
        let before = settings("on", "", Some(""));

        let value = service
            .with_archive_rollback(&before, WALG_COMMAND, "temps-postgres", async {
                Ok::<_, BackupError>(42)
            })
            .await
            .unwrap();

        assert_eq!(value, 42);
        drop(service);
        assert!(logged_sql(db).is_empty());
    }

    #[tokio::test]
    async fn failed_rollback_still_reports_the_backup_failure() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let service = make_scope_test_service(Arc::new(db));
        let before = settings("on", "", Some(""));

        let error = service
            .with_archive_rollback(&before, WALG_COMMAND, "temps-postgres", async {
                Err::<(), _>(push_failure())
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("wal-g backup-push failed"), "{error}");
    }
}
