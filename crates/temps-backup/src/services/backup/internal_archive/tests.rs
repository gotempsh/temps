// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for `internal_archive`.

use super::super::test_support::make_scope_test_service;
use super::*;
use sea_orm::{MockDatabase, MockExecResult};

const WALG_COMMAND: &str = ". /home/postgres/pgdata/walg.env && wal-g wal-push %p";
const AUTO_CONF: &str = "/home/postgres/pgdata/postgresql.auto.conf";

/// Settings whose `archive_command` nobody set.
fn settings(mode: &str, command: &str, library: Option<&str>) -> ArchiveSettings {
    ArchiveSettings {
        archive_mode: mode.to_string(),
        archive_command: command.to_string(),
        archive_library: library.map(str::to_string),
        archive_command_source: Some("default".to_string()),
        archive_command_sourcefile: None,
    }
}

/// Settings whose `archive_command` was set in `sourcefile`.
fn settings_in(command: &str, library: Option<&str>, sourcefile: &str) -> ArchiveSettings {
    ArchiveSettings {
        archive_command_source: Some("configuration file".to_string()),
        archive_command_sourcefile: Some(sourcefile.to_string()),
        ..settings("on", command, library)
    }
}

fn settings_row(settings: &ArchiveSettings) -> BTreeMap<&'static str, Value> {
    let optional = |value: &Option<String>| Value::String(value.clone().map(Box::new));
    BTreeMap::from([
        ("archive_mode", Value::from(settings.archive_mode.as_str())),
        (
            "archive_command",
            Value::from(settings.archive_command.as_str()),
        ),
        ("archive_library", optional(&settings.archive_library)),
        (
            "archive_command_source",
            optional(&settings.archive_command_source),
        ),
        (
            "archive_command_sourcefile",
            optional(&settings.archive_command_sourcefile),
        ),
    ])
}

fn reload_row(reloaded: bool) -> BTreeMap<&'static str, Value> {
    BTreeMap::from([("reloaded", Value::Bool(Some(reloaded)))])
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

fn file_entry_row(sourcefile: &str, setting: &str) -> BTreeMap<&'static str, Value> {
    BTreeMap::from([
        ("sourcefile", Value::from(sourcefile)),
        ("setting", Value::from(setting)),
    ])
}

/// pg_file_settings once Temps wrote the WAL-G command.
fn walg_in_auto_conf() -> Vec<BTreeMap<&'static str, Value>> {
    vec![file_entry_row(AUTO_CONF, WALG_COMMAND)]
}

/// A database answering `queries` in order and accepting `changes`
/// statements (ALTER SYSTEM).
fn mock(
    queries: Vec<Vec<BTreeMap<&'static str, Value>>>,
    changes: usize,
) -> Arc<DatabaseConnection> {
    Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(queries)
            .append_exec_results((0..changes).map(|_| exec_ok()))
            .into_connection(),
    )
}

/// Steps recorded by `archive_then_push` tests, in the order they ran.
type Steps = Arc<std::sync::Mutex<Vec<&'static str>>>;

fn step(steps: &Steps, name: &'static str) {
    steps.lock().unwrap().push(name);
}

type StepFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BackupError>> + 'a>>;

/// Base backup steps that record themselves, with writing walg.env and
/// the push succeeding or failing as asked.
fn recorded_steps(
    steps: &Steps,
    write_ok: bool,
    push_ok: bool,
) -> BaseBackupSteps<StepFuture<'_>, StepFuture<'_>, StepFuture<'_>, StepFuture<'_>> {
    BaseBackupSteps {
        write_env: Box::pin(async move {
            step(steps, "write walg.env");
            if write_ok {
                Ok(())
            } else {
                Err(BackupError::Internal {
                    message: "Failed to write walg.env in container 'temps-postgres'".to_string(),
                })
            }
        }),
        push: Box::pin(async move {
            step(steps, "backup-push");
            if push_ok {
                Ok(())
            } else {
                Err(push_failure())
            }
        }),
        restore_env: Box::pin(async move {
            step(steps, "restore walg.env");
            Ok(())
        }),
        discard_previous_env: Box::pin(async move {
            step(steps, "discard previous walg.env");
            Ok(())
        }),
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
    assert!(!settings("on", WALG_COMMAND, Some("")).retains_wal_without_destination());
    assert!(!settings("on", "", Some("basic_archive")).retains_wal_without_destination());
    assert!(!settings("on", NO_ARCHIVE_DESTINATION_COMMAND, None).retains_wal_without_destination());
}

#[tokio::test]
async fn bundled_database_retaining_wal_gets_a_discarding_archive_command() {
    let retaining = settings_row(&settings("on", "", Some("")));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![retaining.clone()],
            vec![retaining],
            vec![],
            vec![reload_row(true)],
        ])
        .append_exec_results([exec_ok()])
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
    assert_eq!(sql.len(), 5, "unexpected statements: {sql:?}");
    assert!(sql[2].contains("pg_file_settings"), "{sql:?}");
    assert_eq!(sql[3], "ALTER SYSTEM SET archive_command = '/bin/true'");
    assert_eq!(sql[4], "SELECT pg_reload_conf() AS reloaded");
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

    let docker_up = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker.ping().await.is_ok(),
        Err(_) => false,
    };
    if !docker_up {
        println!("Docker not available, skipping");
        return;
    }
    // With Docker up, a container that does not start is a failure.
    let container = GenericImage::new("pgvector/pgvector", "pg18")
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
        .expect("PostgreSQL container failed to start");
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
    // The archiver drops a segment's .ready file before it counts the
    // segment, so wait for both.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while scalar(&db, READY).await > 0 || scalar(&db, ARCHIVED).await < pending {
        assert!(
            std::time::Instant::now() < deadline,
            "segments still waiting for the archiver after 30s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    // A second start leaves the database as it is.
    let outcome = service
        .reconcile_internal_wal_archiving_with(|| async {
            panic!("a released database must not trigger container discovery")
        })
        .await
        .unwrap();
    assert_eq!(outcome, InternalArchiveReconcile::Unaffected);

    // Emptying the command on purpose pauses archiving, and is left alone.
    // pg_reload_conf() only signals the server, so wait until it applies.
    db.execute_unprepared("ALTER SYSTEM SET archive_command = ''")
        .await
        .unwrap();
    db.execute_unprepared("SELECT pg_reload_conf()")
        .await
        .unwrap();
    const STILL_RELEASED: &str = "SELECT count(*) AS n FROM pg_settings \
                                  WHERE name = 'archive_command' AND setting <> ''";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while scalar(&db, STILL_RELEASED).await > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "archive_command was not emptied 10s after the reload"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
        .append_query_results([vec![settings_row(&settings_in(
            WALG_COMMAND,
            Some(""),
            AUTO_CONF,
        ))]])
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
        .append_query_results([vec![settings_row(&settings("on", "", None))]])
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
async fn archiving_set_up_while_the_container_was_located_is_not_replaced() {
    // No exec result is queued: an ALTER SYSTEM would fail the mock.
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![settings_row(&settings("on", "", None))],
            vec![settings_row(&settings_in(
                "cp %p /archive/%f",
                None,
                AUTO_CONF,
            ))],
        ])
        .into_connection();
    let service = make_scope_test_service(Arc::new(db));

    let outcome = service
        .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
        .await
        .unwrap();

    assert_eq!(outcome, InternalArchiveReconcile::Unaffected);
}

#[tokio::test]
async fn archiving_paused_while_the_container_was_located_is_left_to_its_operator() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![settings_row(&settings("on", "", None))],
            vec![settings_row(&settings_in("", None, AUTO_CONF))],
        ])
        .into_connection();
    let service = make_scope_test_service(Arc::new(db));

    let outcome = service
        .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
        .await
        .unwrap();

    assert!(
        matches!(outcome, InternalArchiveReconcile::LeftToOperator { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_command_written_but_not_yet_loaded_is_not_replaced() {
    // ALTER SYSTEM without a reload: the running value is still the
    // default, the file already holds the operator's command. No exec
    // result is queued: an ALTER SYSTEM would fail the mock.
    let retaining = settings_row(&settings("on", "", None));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![retaining.clone()],
            vec![retaining],
            vec![file_entry_row(AUTO_CONF, "cp %p /archive/%f")],
        ])
        .into_connection();
    let service = make_scope_test_service(Arc::new(db));

    let outcome = service
        .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
        .await
        .unwrap();

    assert_eq!(
        outcome,
        InternalArchiveReconcile::LeftToOperator {
            reason: format!(
                "archive_command is set in {AUTO_CONF} but not loaded yet; it applies on \
                 the next configuration reload"
            )
        }
    );
}

#[tokio::test]
async fn configuration_files_that_cannot_be_read_leave_the_database_alone() {
    let retaining = settings_row(&settings("on", "", None));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![retaining.clone()], vec![retaining]])
        .append_query_errors([sea_orm::DbErr::Custom(
            "permission denied for view pg_file_settings".to_string(),
        )])
        .into_connection();
    let service = make_scope_test_service(Arc::new(db));

    let outcome = service
        .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
        .await
        .unwrap();

    let InternalArchiveReconcile::LeftToOperator { reason } = outcome else {
        panic!("expected LeftToOperator, got {outcome:?}");
    };
    assert!(reason.contains("permission denied"), "{reason}");
}

#[tokio::test]
async fn failing_alter_system_names_the_container_and_the_command() {
    let retaining = settings_row(&settings("on", "", Some("")));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![retaining.clone()], vec![retaining], vec![]])
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

#[tokio::test]
async fn a_placeholder_whose_reload_failed_is_reloaded_on_the_next_start() {
    // An earlier start wrote /bin/true but could not reload: the running
    // command is still the default, and only postgresql.auto.conf has it.
    let retaining = settings_row(&settings("on", "", None));
    let db = mock(
        vec![
            vec![retaining.clone()],
            vec![retaining],
            vec![file_entry_row(AUTO_CONF, NO_ARCHIVE_DESTINATION_COMMAND)],
            vec![reload_row(true)],
        ],
        1,
    );
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
    assert_eq!(
        logged_sql(db)[3],
        "ALTER SYSTEM SET archive_command = '/bin/true'"
    );
}

#[tokio::test]
async fn a_pending_placeholder_does_not_hide_a_pending_operator_command() {
    let retaining = settings_row(&settings("on", "", None));
    let db = mock(
        vec![
            vec![retaining.clone()],
            vec![retaining],
            vec![
                file_entry_row("/etc/postgresql/postgresql.conf", "cp %p /archive/%f"),
                file_entry_row(AUTO_CONF, NO_ARCHIVE_DESTINATION_COMMAND),
            ],
        ],
        0,
    );
    let service = make_scope_test_service(db);

    let outcome = service
        .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
        .await
        .unwrap();

    assert!(
        matches!(outcome, InternalArchiveReconcile::LeftToOperator { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_reload_that_cannot_signal_the_server_is_not_a_release() {
    let retaining = settings_row(&settings("on", "", Some("")));
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([
            vec![retaining.clone()],
            vec![retaining],
            vec![],
            vec![reload_row(false)],
        ])
        .append_exec_results([exec_ok()])
        .into_connection();
    let service = make_scope_test_service(Arc::new(db));

    let error = service
        .reconcile_internal_wal_archiving_with(|| async { Ok("temps-postgres".to_string()) })
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("not in effect"), "{error}");
    assert!(error.contains("temps-postgres"), "{error}");
}

#[test]
fn a_container_is_the_connected_database_only_with_the_same_identity() {
    let primary = "7301234567890123456/1763712345.123456";
    assert!(same_instance("temps-postgres", primary, &format!("{primary}\n")).is_ok());

    // Another cluster.
    let error = same_instance(
        "temps-postgres",
        primary,
        "7309999999999999999/1763712345.123456",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("temps-postgres"), "{error}");
    assert!(error.contains("7309999999999999999"), "{error}");

    // A replica of the connected server: same cluster, its own start.
    assert!(same_instance(
        "temps-postgres",
        primary,
        "7301234567890123456/1763799999.654321"
    )
    .is_err());

    // An empty read (psql printed nothing) never counts as a match.
    assert!(same_instance("temps-postgres", "", "").is_err());
}

#[tokio::test]
async fn archiving_paused_on_purpose_is_left_to_its_operator() {
    // Only the settings query is queued: an ALTER SYSTEM would fail the
    // mock with "no exec result".
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![settings_row(&settings_in(
            "",
            Some(""),
            "/etc/postgresql/postgresql.conf",
        ))]])
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
fn a_failed_base_backup_puts_back_what_archive_command_was() {
    let set = |command: &str| Some(ArchiveCommandChange::Set(command.to_string()));
    // Nobody had set it: back to the placeholder, not to retaining WAL.
    assert_eq!(
        settings("on", "", Some("")).rollback(WALG_COMMAND),
        set(NO_ARCHIVE_DESTINATION_COMMAND)
    );
    // The startup check had released it.
    assert_eq!(
        settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF).rollback(WALG_COMMAND),
        set(NO_ARCHIVE_DESTINATION_COMMAND)
    );
    // A pause through ALTER SYSTEM is put back as it was.
    assert_eq!(
        settings_in("", None, AUTO_CONF).rollback(WALG_COMMAND),
        set("")
    );
    // A pause in the operator's own file: Temps' override is removed, so
    // their next edit to that file applies.
    assert_eq!(
        settings_in("", None, "/etc/postgresql/postgresql.conf").rollback(WALG_COMMAND),
        Some(ArchiveCommandChange::Reset)
    );
    // A file PostgreSQL does not show: keep the value that was in effect.
    let hidden = ArchiveSettings {
        archive_command_sourcefile: None,
        ..settings_in("", None, AUTO_CONF)
    };
    assert_eq!(hidden.rollback(WALG_COMMAND), set(""));
    // Archiving with WAL-G was already in place: nothing to undo.
    assert_eq!(
        settings_in(WALG_COMMAND, None, AUTO_CONF).rollback(WALG_COMMAND),
        None
    );
    // The same command from the operator's own file: remove the override
    // the attempt added, so later edits to that file apply.
    assert_eq!(
        settings_in(WALG_COMMAND, None, "/etc/postgresql/conf.d/archive.conf")
            .rollback(WALG_COMMAND),
        Some(ArchiveCommandChange::Reset)
    );
}

#[test]
fn archiving_temps_does_not_manage_is_never_switched_to_walg() {
    let blocked = |settings: ArchiveSettings| settings.walg_takeover_blocker(WALG_COMMAND);
    assert!(blocked(settings_in("cp %p /archive/%f", None, AUTO_CONF)).is_some());
    assert!(blocked(settings("on", "", Some("basic_archive"))).is_some());
    // WAL-G run by someone else, to their own archive.
    assert!(blocked(settings_in(
        "env WALG_S3_PREFIX=s3://operator/archive wal-g wal-push %p",
        None,
        AUTO_CONF
    ))
    .is_some());
    // The managed-service images' command, not the control plane's.
    assert!(blocked(settings_in(
        ". /var/lib/postgresql/walg.env && wal-g wal-push %p",
        None,
        AUTO_CONF
    ))
    .is_some());
    // No archiver runs, so the base backup's WAL would never be shipped.
    let off = blocked(settings("off", "", None)).unwrap();
    assert!(off.contains("archive_mode is 'off'"), "{off}");

    // Temps' own states: unset, the placeholder, paused, its WAL-G command.
    assert_eq!(blocked(settings("on", "", Some(""))), None);
    assert_eq!(
        blocked(settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF)),
        None
    );
    assert_eq!(blocked(settings_in("", None, AUTO_CONF)), None);
    assert_eq!(blocked(settings_in(WALG_COMMAND, None, AUTO_CONF)), None);
}

#[tokio::test]
async fn base_backup_refuses_to_take_over_an_operator_archive() {
    // Nothing is queued: any statement would fail the mock.
    let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings_in("cp %p '/archive/%f'", None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, true),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("Temps does not manage"), "{error}");
    assert!(error.contains("temps-postgres"), "{error}");
    assert!(steps.lock().unwrap().is_empty());
    drop(service);
    assert!(logged_sql(db).is_empty());
}

#[tokio::test]
async fn base_backup_starts_only_after_archiving_is_enabled() {
    let db = mock(vec![vec![], vec![reload_row(true)]], 1);
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    service
        .archive_then_push(
            &settings("on", "", Some("")),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, true),
        )
        .await
        .unwrap();

    assert_eq!(
        *steps.lock().unwrap(),
        ["write walg.env", "backup-push", "discard previous walg.env"]
    );
    drop(service);
    let sql = logged_sql(db);
    assert!(sql[0].contains("pg_file_settings"), "{sql:?}");
    assert_eq!(
        sql[1..],
        [
            format!("ALTER SYSTEM SET archive_command = '{WALG_COMMAND}'"),
            "SELECT pg_reload_conf() AS reloaded".to_string()
        ]
    );
}

#[tokio::test]
async fn walg_env_that_cannot_be_written_changes_no_setting() {
    // Only the configuration files are read: any ALTER SYSTEM would fail
    // the mock.
    let db = mock(vec![vec![]], 0);
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, false, true),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("could not be enabled"), "{error}");
    assert_eq!(
        *steps.lock().unwrap(),
        ["write walg.env", "restore walg.env"]
    );
    drop(service);
    assert_eq!(logged_sql(db).len(), 1);
}

#[tokio::test]
async fn archive_command_that_was_never_written_is_not_rolled_back() {
    // ALTER SYSTEM is refused, so postgresql.auto.conf does not hold the
    // WAL-G command and there is nothing to undo.
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_errors([sea_orm::DbErr::Custom(
                "permission denied to set parameter \"archive_command\"".to_string(),
            )])
            .append_query_results([Vec::<BTreeMap<&'static str, Value>>::new(), Vec::new()])
            .into_connection(),
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, true),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("could not be enabled"), "{error}");
    assert!(error.contains("permission denied"), "{error}");
    assert_eq!(
        *steps.lock().unwrap(),
        ["write walg.env", "restore walg.env"]
    );
    drop(service);
    let sql = logged_sql(db);
    assert!(
        !sql.iter().any(|statement| statement.contains("/bin/true")),
        "{sql:?}"
    );
}

#[tokio::test]
async fn archiving_whose_reload_failed_is_rolled_back_without_a_base_backup() {
    let db = mock(
        vec![
            vec![],
            vec![reload_row(false)],
            walg_in_auto_conf(),
            vec![reload_row(true)],
        ],
        2,
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings("on", "", Some("")),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, true),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("could not be enabled"), "{error}");
    assert!(error.contains("not in effect"), "{error}");
    assert_eq!(
        *steps.lock().unwrap(),
        ["write walg.env", "restore walg.env"]
    );
    drop(service);
    let sql = logged_sql(db);
    assert_eq!(sql[4], "ALTER SYSTEM SET archive_command = '/bin/true'");
    assert_eq!(sql.len(), 6, "{sql:?}");
}

#[tokio::test]
async fn failed_first_base_backup_puts_back_the_placeholder_and_walg_env() {
    let db = mock(
        vec![
            vec![],
            vec![reload_row(true)],
            walg_in_auto_conf(),
            vec![reload_row(true)],
        ],
        2,
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, false),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("wal-g backup-push failed"), "{error}");
    assert_eq!(
        *steps.lock().unwrap(),
        ["write walg.env", "backup-push", "restore walg.env"]
    );
    drop(service);
    let sql = logged_sql(db);
    assert_eq!(
        sql[4..],
        [
            "ALTER SYSTEM SET archive_command = '/bin/true'",
            "SELECT pg_reload_conf() AS reloaded"
        ]
    );
}

#[tokio::test]
async fn failed_base_backup_removes_the_override_of_a_pause_in_the_operator_file() {
    let db = mock(
        vec![
            vec![],
            vec![reload_row(true)],
            walg_in_auto_conf(),
            vec![reload_row(true)],
        ],
        2,
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let result = service
        .archive_then_push(
            &settings_in("", None, "/etc/postgresql/postgresql.conf"),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, false),
        )
        .await;

    assert!(result.is_err());
    drop(service);
    assert_eq!(logged_sql(db)[4], "ALTER SYSTEM RESET archive_command");
}

#[tokio::test]
async fn failed_base_backup_leaves_a_command_written_since_by_someone_else() {
    // Only the enabling change is accepted: a rollback would fail the mock.
    let db = mock(
        vec![
            vec![],
            vec![reload_row(true)],
            vec![
                file_entry_row(AUTO_CONF, WALG_COMMAND),
                file_entry_row(AUTO_CONF, "cp %p /archive/%f"),
            ],
        ],
        1,
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, false),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("wal-g backup-push failed"), "{error}");
    drop(service);
    assert_eq!(logged_sql(db).len(), 4);
}

/// A database whose pg_file_settings reads fail `failures` times while a
/// failed backup checks it still owns archive_command, then show the WAL-G
/// command, and that accepts a rollback.
fn ownership_read_fails(failures: usize) -> Arc<DatabaseConnection> {
    let permission_denied =
        || sea_orm::DbErr::Custom("permission denied for view pg_file_settings".to_string());
    Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![], vec![reload_row(true)]])
            .append_query_errors((0..failures).map(|_| permission_denied()))
            .append_query_results([walg_in_auto_conf(), vec![reload_row(true)]])
            .append_exec_results([exec_ok(), exec_ok()])
            .into_connection(),
    )
}

#[tokio::test]
async fn failed_base_backup_is_rolled_back_once_ownership_is_confirmed_on_a_retry() {
    let db = ownership_read_fails(1);
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let result = service
        .archive_then_push(
            &settings("on", "", None),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, false),
        )
        .await;

    assert!(result.is_err());
    drop(service);
    let sql = logged_sql(db);
    assert_eq!(
        sql.iter()
            .filter(|statement| statement.contains("pg_file_settings"))
            .count(),
        3,
        "{sql:?}"
    );
    assert!(
        sql.contains(&"ALTER SYSTEM SET archive_command = '/bin/true'".to_string()),
        "{sql:?}"
    );
}

#[tokio::test]
async fn failed_base_backup_is_not_rolled_back_while_ownership_is_unknown() {
    // Every check fails: writing blind could replace a command someone set
    // since with one that discards WAL.
    let db = ownership_read_fails(3);
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings("on", "", None),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, false),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("wal-g backup-push failed"), "{error}");
    drop(service);
    let sql = logged_sql(db);
    assert!(
        !sql.iter().any(|statement| statement.contains("/bin/true")),
        "{sql:?}"
    );
}

#[tokio::test]
async fn base_backup_leaves_a_command_staged_but_not_loaded_yet() {
    // The running command is the placeholder; postgresql.auto.conf already
    // holds the operator's. Nothing else is queued: an ALTER SYSTEM would
    // fail the mock.
    let db = mock(
        vec![vec![
            file_entry_row(AUTO_CONF, NO_ARCHIVE_DESTINATION_COMMAND),
            file_entry_row(AUTO_CONF, "cp %p /archive/%f"),
        ]],
        0,
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings_in(NO_ARCHIVE_DESTINATION_COMMAND, None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, true),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("not loaded"), "{error}");
    assert!(!error.contains("/archive/"), "{error}");
    assert!(steps.lock().unwrap().is_empty());
    drop(service);
    assert_eq!(logged_sql(db).len(), 1);
}

#[tokio::test]
async fn base_backup_needs_the_configuration_files_to_be_readable() {
    let db = Arc::new(
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom(
                "permission denied for view pg_file_settings".to_string(),
            )])
            .into_connection(),
    );
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let error = service
        .archive_then_push(
            &settings("on", "", None),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, true),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("permission denied"), "{error}");
    assert!(steps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_base_backup_keeps_walg_archiving_but_puts_back_its_walg_env() {
    // walg.env is what tells WAL-G where the working archive is; the
    // command was already WAL-G's, so only the enabling change runs.
    let db = mock(vec![vec![], vec![reload_row(true)]], 1);
    let service = make_scope_test_service(db.clone());
    let steps = Steps::default();

    let result = service
        .archive_then_push(
            &settings_in(WALG_COMMAND, None, AUTO_CONF),
            WALG_COMMAND,
            "temps-postgres",
            recorded_steps(&steps, true, false),
        )
        .await;

    assert!(result.is_err());
    assert_eq!(
        *steps.lock().unwrap(),
        ["write walg.env", "backup-push", "restore walg.env"]
    );
    drop(service);
    assert_eq!(logged_sql(db).len(), 3);
}

#[tokio::test]
async fn successful_base_backup_returns_the_push_result() {
    let db = mock(vec![vec![], vec![reload_row(true)]], 1);
    let service = make_scope_test_service(db.clone());

    let value = service
        .archive_then_push(
            &settings("on", "", Some("")),
            WALG_COMMAND,
            "temps-postgres",
            BaseBackupSteps {
                write_env: async { Ok(()) },
                push: async { Ok::<_, BackupError>(42) },
                restore_env: async { panic!("a successful backup keeps the new walg.env") },
                discard_previous_env: async {
                    Err(BackupError::Internal {
                        message: "rm: cannot remove walg.env.prev".to_string(),
                    })
                },
            },
        )
        .await
        .unwrap();

    // Failing to delete the copy of the old walg.env does not fail it.
    assert_eq!(value, 42);
}

#[tokio::test]
async fn failed_rollback_still_reports_the_backup_failure() {
    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_exec_results([exec_ok()])
        .append_exec_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
        .append_query_results([vec![], vec![reload_row(true)], walg_in_auto_conf()])
        .into_connection();
    let service = make_scope_test_service(Arc::new(db));

    let error = service
        .archive_then_push(
            &settings("on", "", Some("")),
            WALG_COMMAND,
            "temps-postgres",
            BaseBackupSteps {
                write_env: async { Ok(()) },
                push: async { Err::<(), _>(push_failure()) },
                restore_env: async {
                    Err(BackupError::Internal {
                        message: "mv: cannot move walg.env.prev".to_string(),
                    })
                },
                discard_previous_env: async { panic!("a failed backup keeps the old walg.env") },
            },
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("wal-g backup-push failed"), "{error}");
}
