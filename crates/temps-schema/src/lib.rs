// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! This binary's database schema: the startup compatibility guard, the
//! migration runner and post-migration maintenance.
//!
//! The only crate besides test support that depends on `temps-migrations`, so
//! that adding a migration rebuilds this crate and the binary, not every crate
//! that opens a database connection.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
mod error;
pub mod lock_capacity;
mod maintenance;
mod migrations;
pub mod schema_guard;

pub use error::{SchemaError, SchemaResult};
pub use lock_capacity::{read_lock_capacity, LockCapacity, LockSlotsExhausted};
pub use maintenance::{
    detach_telemetry_foreign_keys, reconcile_otel_trace_summaries,
    remaining_telemetry_foreign_keys, run_post_migration_backfill,
    run_post_migration_backfill_streaming, run_post_migration_indexes,
    run_post_migration_indexes_streaming, run_vault_audit_backfill, MaintenanceProgress,
    TelemetryForeignKey, VaultAuditBackfillSummary, DETACH_TELEMETRY_FOREIGN_KEYS_STEP,
};
pub use migrations::{
    defined_migration_count, ensure_no_unprotected_upgrade, establish_connection,
    get_pending_migration_names, run_migrations, run_migrations_reported, run_migrations_streaming,
    MigrationProgress, MigrationRunReport, MigrationStepResult,
};
pub use schema_guard::{check_schema_compatibility, SchemaGuardError, SchemaStatus};

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use sea_orm_migration::MigratorTrait;
    use temps_database::{connect_without_migrations, DbConnection};
    use temps_migrations::Migrator;
    use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

    /// Retry a container connection attempt with exponential backoff.
    ///
    /// TimescaleDB containers on a loaded CI runner can take much longer than
    /// a few seconds to finish initdb and start accepting connections; a
    /// connection attempted too early observes "Connection reset by peer"
    /// rather than a connection-refused, and a fixed 5-attempt/2s budget
    /// (~13s total) was repeatedly too short under CI resource contention.
    /// Retry for up to 60s with backoff instead of a fixed attempt count.
    async fn retry_db_connect<T, E, F, Fut>(mut connect_fn: F) -> anyhow::Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(60);
        let mut delay = tokio::time::Duration::from_millis(250);
        let max_delay = tokio::time::Duration::from_secs(3);
        loop {
            match connect_fn().await {
                Ok(value) => return Ok(value),
                Err(e) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(anyhow::anyhow!(
                            "Failed to connect to database after retrying for 60s: {}",
                            e
                        ));
                    }
                    tokio::time::sleep(delay).await;
                    delay = std::cmp::min(delay * 2, max_delay);
                }
            }
        }
    }

    /// Print the part of the PostgreSQL server log that explains a failed
    /// statement.
    ///
    /// The client only sees `deadlock detected`; the server log is where
    /// PostgreSQL names both sessions and the statements they were running,
    /// and it is gone as soon as the container is dropped. Print the report
    /// when there is one, otherwise the tail of the log.
    async fn print_server_log(container: &testcontainers::ContainerAsync<GenericImage>) {
        let raw = match container.stderr_to_vec().await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                eprintln!("Could not read the PostgreSQL server log: {error}");
                return;
            }
        };
        let lines: Vec<&str> = raw.lines().collect();
        let excerpt = match lines
            .iter()
            .position(|line| line.contains("deadlock detected"))
        {
            Some(at) => &lines[at.saturating_sub(2)..(at + 14).min(lines.len())],
            None => &lines[lines.len().saturating_sub(40)..],
        };
        eprintln!("--- PostgreSQL server log ---\n{}", excerpt.join("\n"));
    }

    #[tokio::test]
    async fn test_establish_connection() -> anyhow::Result<()> {
        // Start TimescaleDB container
        let postgres_container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping Docker-dependent database test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        // Connect with retries; the container may still be finishing initdb.
        let db = retry_db_connect(|| Database::connect(&database_url)).await?;

        // Test basic connectivity
        let result = sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT 1".to_owned(),
        );

        let query_result = db.query_one(result).await?;
        assert!(query_result.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_establish_connection_with_migrations() -> anyhow::Result<()> {
        // Start TimescaleDB container
        let postgres_container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping Docker-dependent database test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        // Connect with retries; the container may still be finishing initdb.
        let connection = retry_db_connect(|| establish_connection(&database_url)).await?;

        // Heavy audit index creation is deliberately outside the migration
        // transaction. Prove the post-migration path is idempotent and creates
        // the expected partial concurrent index against a real PostgreSQL.
        for _ in 0..2 {
            if let Err(error) = run_post_migration_indexes(connection.as_ref()).await {
                print_server_log(&postgres_container).await;
                return Err(error.into());
            }
        }
        let index = connection
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT indexdef FROM pg_indexes \
                 WHERE indexname = 'idx_audit_logs_permission_denied_retention'"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("permission-denied retention index was not created"))?;
        let index_definition: String = index.try_get("", "indexdef")?;
        assert!(index_definition.contains("audit_date"));
        // PostgreSQL may render the predicate with type casts and additional
        // parentheses, so assert its semantic pieces instead of one formatting.
        assert!(index_definition.contains("operation_type"));
        assert!(index_definition.contains("PERMISSION_DENIED"));

        connection
            .execute_unprepared(
                "DROP INDEX CONCURRENTLY idx_audit_logs_permission_denied_retention",
            )
            .await?;
        connection
            .execute_unprepared(
                "CREATE FUNCTION fail_permission_index(integer) RETURNS integer \
                 IMMUTABLE LANGUAGE plpgsql AS $$ \
                 BEGIN RAISE EXCEPTION 'intentional index build failure'; END $$; \
                 CREATE TABLE invalid_permission_index_fixture(id integer); \
                 INSERT INTO invalid_permission_index_fixture VALUES (1);",
            )
            .await?;
        let failed_build = connection
            .execute_unprepared(
                "CREATE INDEX CONCURRENTLY idx_audit_logs_permission_denied_retention \
                 ON invalid_permission_index_fixture (fail_permission_index(id))",
            )
            .await;
        assert!(failed_build.is_err());
        let invalid = connection
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT index.indisvalid AS is_valid FROM pg_index AS index \
                 WHERE index.indexrelid = \
                     to_regclass('idx_audit_logs_permission_denied_retention')"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed concurrent build left no index identity"))?;
        assert!(!invalid.try_get::<bool>("", "is_valid")?);

        run_post_migration_indexes(connection.as_ref()).await?;
        let repaired = connection
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT index.indisvalid AS is_valid FROM pg_index AS index \
                 WHERE index.indexrelid = \
                     to_regclass('idx_audit_logs_permission_denied_retention')"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("invalid retention index was not rebuilt"))?;
        assert!(repaired.try_get::<bool>("", "is_valid")?);

        // The compatibility migration is deliberately reversible as a no-op:
        // the concurrently managed index must survive both down and re-up.
        //
        // Compute how many migrations exist AFTER
        // `m20260806_000001_index_permission_denied_retention` so we roll back
        // exactly (count + 1) migrations to land just before it. Hardcoding
        // `Some(1)` broke whenever a new migration was appended after this one
        // — that new migration would be the one rolled back instead of the
        // index-retention migration we're actually testing.
        let target = "m20260806_000001_index_permission_denied_retention";
        let all_migrations = Migrator::migrations();
        let target_pos = all_migrations
            .iter()
            .position(|m| m.name() == target)
            .ok_or_else(|| anyhow::anyhow!("{} not found in migration list", target))?;
        let after_count = all_migrations.len() - target_pos - 1;
        let steps = (after_count + 1) as u32;
        Migrator::down(connection.as_ref(), Some(steps)).await?;
        let pending_after_down = get_pending_migration_names(connection.as_ref()).await?;
        // Pending migrations are returned in ascending (chronological) order —
        // the target is the oldest pending migration, so it is first().
        // Rolling back more than 1 step means later migrations appear after it.
        assert_eq!(
            pending_after_down.first().map(String::as_str),
            Some(target),
            "expected first pending migration to be '{}' after rolling back {} steps (after_count={}), got: {:?}",
            target,
            steps,
            after_count,
            pending_after_down,
        );
        Migrator::up(connection.as_ref(), Some(steps)).await?;
        let index_after_round_trip = connection
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT index.indisvalid AS is_valid FROM pg_index AS index \
                 WHERE index.indexrelid = \
                     to_regclass('idx_audit_logs_permission_denied_retention')"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("migration round-trip removed managed index"))?;
        assert!(index_after_round_trip.try_get::<bool>("", "is_valid")?);

        // If we get here, migrations ran successfully and connection is established
        println!("✅ Database connection with migrations established successfully");

        Ok(())
    }

    /// A database behind this binary's migrations, for tests of the guard in
    /// [`establish_connection`]. Its ledger holds only the first migration, so
    /// the rest are pending. `None` when no container runtime is available.
    async fn database_with_pending_migrations(
    ) -> anyhow::Result<Option<temps_test_utils::TestDatabase>> {
        let test_db = match temps_test_utils::TestDatabase::new().await {
            Ok(test_db) => test_db,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping schema guard startup test: {error}");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let first = schema_guard::known_migration_names().remove(0);
        test_db
            .db
            .execute_unprepared(&format!(
                "CREATE TABLE seaql_migrations (version varchar PRIMARY KEY, applied_at bigint NOT NULL); \
                 INSERT INTO seaql_migrations VALUES ('{first}', 0);"
            ))
            .await?;
        Ok(Some(test_db))
    }

    /// The guard runs before any migration: an existing database with pending
    /// migrations is refused by `establish_connection` and its ledger is left
    /// exactly as it was found.
    #[tokio::test]
    async fn establish_connection_refuses_an_unprotected_upgrade() -> anyhow::Result<()> {
        let Some(test_db) = database_with_pending_migrations().await? else {
            return Ok(());
        };

        let error = establish_connection(&test_db.database_url)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("a pending upgrade must be refused"))?;
        assert!(
            error.to_string().contains("Run `temps migrate`"),
            "unexpected error: {error}"
        );

        let ledger = test_db
            .query_sql("SELECT version FROM seaql_migrations")
            .await?;
        assert_eq!(ledger.len(), 1, "the refused start changed the ledger");
        Ok(())
    }

    /// `temps migrate` deliberately connects without the guard: the same
    /// database is not refused on that path, and is told apart as an upgrade.
    #[tokio::test]
    async fn connecting_without_migrations_leaves_the_upgrade_to_the_caller() -> anyhow::Result<()>
    {
        let Some(test_db) = database_with_pending_migrations().await? else {
            return Ok(());
        };

        let db = connect_without_migrations(&test_db.database_url).await?;
        assert!(matches!(
            check_schema_compatibility(db.as_ref()).await?,
            SchemaStatus::PendingUpgrade { applied: 1, .. }
        ));
        Ok(())
    }

    /// A database migrated by a newer release still starts: the guard reports
    /// it rather than refusing, and the unknown migration stays recorded.
    #[tokio::test]
    async fn establish_connection_starts_on_a_database_from_a_newer_release() -> anyhow::Result<()>
    {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(test_db) => test_db,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                eprintln!("Skipping schema guard startup test: {error}");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        const FUTURE: &str = "m29991231_000001_from_a_newer_release";
        test_db
            .execute_sql(&format!(
                "INSERT INTO seaql_migrations (version, applied_at) VALUES ('{FUTURE}', 0)"
            ))
            .await?;

        let db = establish_connection(&test_db.database_url).await?;

        assert!(matches!(
            check_schema_compatibility(db.as_ref()).await?,
            SchemaStatus::NewerThanBinary { ref unknown, ref pending, .. }
                if unknown == &vec![FUTURE.to_string()] && pending.is_empty()
        ));
        Ok(())
    }

    /// `run_migrations_streaming` against a fresh DB must:
    ///   1. emit a `Started` then a `Finished` for every migration, in order,
    ///   2. report every planned migration as applied successfully,
    ///   3. leave the DB up to date so a second run is a no-op (no events),
    /// and `run_migrations_reported` (the wrapper) must agree with it.
    #[tokio::test]
    async fn test_run_migrations_streaming_emits_per_migration_progress() -> anyhow::Result<()> {
        let Some((_container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        let db = db.as_ref();

        // Record the progress events in order so we can assert on the sequence.
        let mut events: Vec<(String, bool)> = Vec::new(); // (kind, success-if-finished)
        let report = run_migrations_streaming(db, |p| match p {
            MigrationProgress::Started { index, total, name } => {
                assert!(index >= 1 && index <= total, "index out of range");
                events.push((format!("started:{name}"), true));
            }
            MigrationProgress::Finished { result, .. } => {
                events.push((format!("finished:{}", result.name), result.success));
            }
        })
        .await?;

        assert!(
            !report.planned.is_empty(),
            "a fresh DB should have pending migrations"
        );
        assert!(
            report.all_succeeded(),
            "all migrations should apply: {report:?}"
        );
        assert!(report.failed().is_none());

        // Every migration must produce exactly one Started immediately followed
        // by its matching Finished, in planned order.
        assert_eq!(events.len(), report.planned.len() * 2);
        for (i, name) in report.planned.iter().enumerate() {
            assert_eq!(events[i * 2].0, format!("started:{name}"));
            assert_eq!(events[i * 2 + 1].0, format!("finished:{name}"));
            assert!(events[i * 2 + 1].1, "{name} should have succeeded");
        }

        // A second run is a clean no-op: nothing pending, no events fired.
        let mut second_run_events = 0usize;
        let report2 = run_migrations_streaming(db, |_| second_run_events += 1).await?;
        assert!(
            report2.planned.is_empty(),
            "second run should be up to date"
        );
        assert_eq!(
            second_run_events, 0,
            "no progress events when nothing pending"
        );

        // And the plain `get_pending_migration_names` agrees there's nothing left.
        assert!(get_pending_migration_names(db).await?.is_empty());

        Ok(())
    }

    /// An older binary started against a database a newer release migrated
    /// (a binary-only rollback) must start: the guard warns, the migration
    /// entry points skip Sea-ORM's migrator (which rejects unknown ledger
    /// entries), nothing is rolled back, and a migration this binary has but
    /// the database lacks (a divergent build) is still applied.
    #[tokio::test]
    async fn migrations_continue_on_a_database_migrated_by_a_newer_release() -> anyhow::Result<()> {
        let Some((_container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        let db = db.as_ref();
        const FUTURE: &str = "m29991231_000001_from_a_newer_release";

        run_migrations(db).await?;
        db.execute_unprepared(&format!(
            "INSERT INTO seaql_migrations (version, applied_at) VALUES ('{FUTURE}', 0)"
        ))
        .await?;

        // Plain rollback: nothing of ours is pending, every entry point succeeds.
        assert!(matches!(
            check_schema_compatibility(db).await?,
            SchemaStatus::NewerThanBinary { ref unknown, ref pending, .. }
                if unknown == &vec![FUTURE.to_string()] && pending.is_empty()
        ));
        ensure_no_unprotected_upgrade(db).await?;
        run_migrations(db).await?;
        assert!(get_pending_migration_names(db).await?.is_empty());
        let report = run_migrations_streaming(db, |_| {}).await?;
        assert!(report.planned.is_empty(), "{report:?}");

        // Divergent build: roll back our newest migration (while the ledger
        // holds only known entries, as Sea-ORM's `down` requires), then add the
        // unknown entry back. Startup must re-apply ours around the unknown one.
        db.execute_unprepared(&format!(
            "DELETE FROM seaql_migrations WHERE version = '{FUTURE}'"
        ))
        .await?;
        Migrator::down(db, Some(1)).await?;
        let newest = schema_guard::known_migration_names()
            .pop()
            .ok_or_else(|| anyhow::anyhow!("binary defines no migrations"))?;
        db.execute_unprepared(&format!(
            "INSERT INTO seaql_migrations (version, applied_at) VALUES ('{FUTURE}', 0)"
        ))
        .await?;
        assert_eq!(get_pending_migration_names(db).await?, vec![newest.clone()]);

        let report = run_migrations_streaming(db, |_| {}).await?;
        assert_eq!(report.planned, vec![newest.clone()]);
        assert!(report.all_succeeded(), "{report:?}");
        assert!(get_pending_migration_names(db).await?.is_empty());

        let ledger: Vec<String> = db
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT version FROM seaql_migrations".to_owned(),
            ))
            .await?
            .iter()
            .map(|row| row.try_get::<String>("", "version"))
            .collect::<Result<_, _>>()?;
        assert!(ledger.contains(&FUTURE.to_string()), "unknown entry kept");
        assert!(ledger.contains(&newest), "own migration recorded");
        Ok(())
    }

    /// Two nodes starting at once against a database a newer release migrated
    /// (e.g. one rolled back, one restarted) must not corrupt the ledger: in
    /// the divergent case this binary's missing migration is recorded exactly
    /// once and a losing start fails cleanly and succeeds on retry; in the
    /// plain-rollback case both starts succeed and write nothing.
    #[tokio::test]
    async fn concurrent_starts_on_a_newer_schema_keep_the_ledger_consistent() -> anyhow::Result<()>
    {
        let Some((container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        let port = container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);
        let db = db.as_ref();
        const FUTURE: &str = "m29991231_000001_from_a_newer_release";

        async fn ledger(db: &DbConnection) -> anyhow::Result<Vec<String>> {
            let mut versions = db
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT version FROM seaql_migrations".to_owned(),
                ))
                .await?
                .iter()
                .map(|row| row.try_get::<String>("", "version"))
                .collect::<Result<Vec<String>, _>>()?;
            versions.sort();
            Ok(versions)
        }

        // Divergent build: our newest migration is missing, a future one is present.
        run_migrations(db).await?;
        Migrator::down(db, Some(1)).await?;
        let newest = schema_guard::known_migration_names()
            .pop()
            .ok_or_else(|| anyhow::anyhow!("binary defines no migrations"))?;
        db.execute_unprepared(&format!(
            "INSERT INTO seaql_migrations (version, applied_at) VALUES ('{FUTURE}', 0)"
        ))
        .await?;

        let node_a = connect_without_migrations(&database_url).await?;
        let node_b = connect_without_migrations(&database_url).await?;
        let (a, b) = tokio::join!(
            run_migrations(node_a.as_ref()),
            run_migrations(node_b.as_ref())
        );
        println!("divergent concurrent starts: a={a:?} b={b:?}");
        assert!(
            a.is_ok() || b.is_ok(),
            "one start must win: a={a:?} b={b:?}"
        );
        // A losing start is retried by the service manager; the retry is clean.
        run_migrations(db).await?;
        assert!(get_pending_migration_names(db).await?.is_empty());
        let versions = ledger(db).await?;
        assert_eq!(
            versions.iter().filter(|v| **v == newest).count(),
            1,
            "own migration recorded exactly once"
        );
        assert!(versions.contains(&FUTURE.to_string()), "unknown entry kept");

        // Plain rollback: nothing of ours is pending, so concurrent starts both
        // succeed and leave the ledger untouched.
        let before = ledger(db).await?;
        let (a, b) = tokio::join!(
            run_migrations(node_a.as_ref()),
            run_migrations(node_b.as_ref())
        );
        a?;
        b?;
        let after = ledger(db).await?;
        assert_eq!(before, after, "concurrent no-op starts changed the ledger");
        Ok(())
    }

    /// Start a throwaway TimescaleDB container and connect (no auto-migrate).
    /// Returns `None` when Docker is unavailable so callers skip gracefully.
    /// A reference index build that loses a lock race must be retried, not
    /// reported as a failure.
    ///
    /// `CREATE INDEX CONCURRENTLY` waits for every transaction that holds a
    /// write lock on the table, and PostgreSQL ends the wait with a deadlock
    /// or a lock timeout when it cannot be granted. Both are transient. Here a
    /// transaction keeps a write lock on `visitor` for longer than the
    /// maintenance connection's 5 s lock timeout, so the first attempt at
    /// `idx_visitor_last_seen` must fail; once the lock is released the
    /// retry has to build the index, which also requires removing the invalid
    /// leftover the failed attempt created.
    #[tokio::test]
    async fn test_post_migration_indexes_retry_a_transient_lock_timeout() -> anyhow::Result<()> {
        use sea_orm::TransactionTrait;

        let Some((container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        run_migrations(db.as_ref()).await?;

        let blocker = db.begin().await?;
        blocker
            .execute_unprepared("LOCK TABLE visitor IN ROW EXCLUSIVE MODE")
            .await?;

        let builder = {
            let db = db.clone();
            tokio::spawn(async move { run_post_migration_indexes(db.as_ref()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(6500)).await;
        blocker.commit().await?;

        let outcome = builder.await?;
        if outcome.is_err() {
            print_server_log(&container).await;
        }
        outcome?;

        let state = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT indisvalid AS is_valid FROM pg_index \
                 WHERE indexrelid = to_regclass('idx_visitor_last_seen')"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("idx_visitor_last_seen was not created"))?;
        assert!(
            state.try_get::<bool>("", "is_valid")?,
            "idx_visitor_last_seen must be valid after the retry"
        );
        Ok(())
    }

    /// Every foreign key column under `projects` ends up with a valid index
    /// leading with it, built after the migrations; a column some other index
    /// already covers gets no duplicate.
    #[tokio::test]
    async fn test_post_migration_indexes_cover_project_foreign_keys() -> anyhow::Result<()> {
        let Some((_container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        run_migrations(db.as_ref()).await?;

        let uncovered_sql = |targets: &[(&str, &str)]| {
            let rows: Vec<String> = targets
                .iter()
                .map(|(table, column)| format!("('{table}', '{column}')"))
                .collect();
            format!(
                "SELECT t.tbl || '.' || t.col AS target \
                 FROM (VALUES {}) AS t(tbl, col) \
                 JOIN pg_attribute a ON a.attrelid = to_regclass(t.tbl) \
                   AND a.attname = t.col AND NOT a.attisdropped \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM pg_index i \
                     WHERE i.indrelid = a.attrelid AND i.indkey[0] = a.attnum \
                       AND i.indisvalid AND i.indpred IS NULL)",
                rows.join(", ")
            )
        };
        let uncovered = |db: std::sync::Arc<DbConnection>| async move {
            let rows = db
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    uncovered_sql(&temps_migrations::PROJECT_FK_INDEXES),
                ))
                .await?;
            rows.iter()
                .map(|row| row.try_get::<String>("", "target"))
                .collect::<Result<Vec<_>, _>>()
        };

        // The migration leaves them to the post-migration build.
        let before = uncovered(db.clone()).await?;
        assert!(
            before.contains(&"performance_metrics.project_id".to_string()),
            "the migration must not build indexes inside its transaction: {before:?}"
        );

        run_post_migration_indexes(db.as_ref()).await?;
        assert_eq!(uncovered(db.clone()).await?, Vec::<String>::new());

        // A column already covered by another index is left alone.
        db.execute_unprepared(
            "DROP INDEX idx_crons_project_id_fk; \
             CREATE INDEX crons_project_and_id ON crons (project_id, id)",
        )
        .await?;
        run_post_migration_indexes(db.as_ref()).await?;
        let rebuilt = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT to_regclass('idx_crons_project_id_fk') IS NOT NULL AS present".to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("to_regclass returned no row"))?;
        assert!(!rebuilt.try_get::<bool>("", "present")?);

        // A partial index doesn't serve `WHERE project_id = $1`, so it is not
        // coverage and the full index is built next to it.
        db.execute_unprepared(
            "DROP INDEX crons_project_and_id; \
             CREATE INDEX crons_project_partial ON crons (project_id) WHERE project_id > 0",
        )
        .await?;
        run_post_migration_indexes(db.as_ref()).await?;
        assert_eq!(uncovered(db.clone()).await?, Vec::<String>::new());
        let built = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT to_regclass('idx_crons_project_id_fk') IS NOT NULL AS present".to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("to_regclass returned no row"))?;
        assert!(built.try_get::<bool>("", "present")?);

        // A partial index that already holds the generated name must not
        // satisfy `IF NOT EXISTS`: it is replaced by the full index.
        db.execute_unprepared(
            "DROP INDEX crons_project_partial; \
             DROP INDEX idx_crons_project_id_fk; \
             CREATE INDEX idx_crons_project_id_fk ON crons (project_id) WHERE project_id > 0",
        )
        .await?;
        run_post_migration_indexes(db.as_ref()).await?;
        assert_eq!(uncovered(db.clone()).await?, Vec::<String>::new());
        let replaced = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT indpred IS NULL AS is_full FROM pg_index \
                 WHERE indexrelid = to_regclass('idx_crons_project_id_fk')"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("idx_crons_project_id_fk was not rebuilt"))?;
        assert!(replaced.try_get::<bool>("", "is_full")?);
        Ok(())
    }

    /// Two processes must not build the project foreign key indexes at once:
    /// while another session holds the lock, maintenance waits (past its 5 s
    /// lock timeout) without building anything, then finishes the set.
    #[tokio::test]
    async fn test_project_foreign_key_indexes_wait_for_another_builder() -> anyhow::Result<()> {
        use sea_orm::TransactionTrait;

        let Some((container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        run_migrations(db.as_ref()).await?;

        let index_present = |db: std::sync::Arc<DbConnection>| async move {
            let row = db
                .query_one(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT to_regclass('idx_performance_metrics_project_id_fk') IS NOT NULL \
                     AS present"
                        .to_owned(),
                ))
                .await?
                .ok_or_else(|| anyhow::anyhow!("to_regclass returned no row"))?;
            Ok::<bool, anyhow::Error>(row.try_get::<bool>("", "present")?)
        };

        let other_builder = db.begin().await?;
        other_builder
            .execute_unprepared(
                "SELECT pg_advisory_xact_lock(hashtext('temps:project_fk_indexes')::BIGINT)",
            )
            .await?;

        let maintenance = {
            let db = db.clone();
            tokio::spawn(async move { run_post_migration_indexes(db.as_ref()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(6500)).await;
        assert!(
            !maintenance.is_finished(),
            "maintenance must wait for the other builder, not fail or skip the lock"
        );
        assert!(!index_present(db.clone()).await?);

        other_builder.commit().await?;
        let outcome = maintenance.await?;
        if outcome.is_err() {
            print_server_log(&container).await;
        }
        outcome?;
        assert!(index_present(db.clone()).await?);
        Ok(())
    }

    /// The foreign key drop `m20261105_000001_project_data_purges` used to run
    /// in `up()`: every telemetry foreign key in one statement, inside the
    /// migration's transaction.
    fn old_project_data_purges_drop_sql() -> String {
        let array = |tables: &[&str]| {
            let quoted: Vec<String> = tables.iter().map(|t| format!("'{t}'")).collect();
            format!("ARRAY[{}]::text[]", quoted.join(", "))
        };
        format!(
            "DO $$ DECLARE fk record; BEGIN \
             FOR fk IN SELECT c.conname, c.conrelid::regclass AS child FROM pg_constraint c \
                 WHERE c.contype = 'f' \
                   AND c.conrelid IN (SELECT to_regclass(t) FROM unnest({children}) AS t) \
                   AND c.confrelid IN (SELECT to_regclass(t) FROM unnest({parents}) AS t) \
             LOOP EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', fk.child, fk.conname); \
             END LOOP; END $$",
            children = array(&temps_migrations::PROJECT_DATA_PURGED_TABLES),
            parents = array(&temps_migrations::PROJECT_DATA_DETACHED_PARENTS),
        )
    }

    const PROJECT_DATA_PURGES: &str = "m20261105_000001_project_data_purges";

    /// Apply every migration before `PROJECT_DATA_PURGES`, as on an instance
    /// still running the release before it.
    async fn migrate_up_to_project_data_purges(db: &DbConnection) -> anyhow::Result<()> {
        let position = Migrator::migrations()
            .iter()
            .position(|migration| migration.name() == PROJECT_DATA_PURGES)
            .ok_or_else(|| anyhow::anyhow!("{PROJECT_DATA_PURGES} is not defined"))?;
        Migrator::up(db, Some(position as u32)).await?;
        Ok(())
    }

    /// Insert one row per day and space partition into `table`, `days` days
    /// back, so it has `days x partitions` chunks. Every NOT NULL column
    /// without a default gets a placeholder of its type, and foreign keys are
    /// not checked (`session_replication_role = replica`): only the chunks
    /// matter. A few days per transaction, so creating them fits the lock
    /// table the test then exhausts.
    async fn fill_chunks(
        db: &DbConnection,
        table: &str,
        days: u32,
        partitions: u32,
    ) -> anyhow::Result<()> {
        const DAYS_PER_BATCH: u32 = 5;
        let mut first = 0;
        while first < days {
            let last = (first + DAYS_PER_BATCH).min(days) - 1;
            db.execute_unprepared(&format!(
                "DO $$ DECLARE cols text; vals text; BEGIN \
                 PERFORM set_config('session_replication_role', 'replica', true); \
                 SELECT string_agg(quote_ident(a.attname), ', ' ORDER BY a.attnum), \
                        string_agg('(' || CASE \
                            WHEN a.attname = 'timestamp' THEN 'now() - make_interval(days => d)' \
                            WHEN a.attname = 'id' THEN '(d * {partitions} + p + 1)' \
                            WHEN t.typcategory = 'N' THEN '1' \
                            WHEN t.typcategory = 'B' THEN 'false' \
                            WHEN t.typcategory = 'D' THEN 'now()' \
                            WHEN t.typname IN ('json', 'jsonb') THEN '''{{}}''' \
                            WHEN t.typname = 'uuid' THEN 'gen_random_uuid()' \
                            WHEN t.typcategory = 'A' THEN '''{{}}''' \
                            ELSE '''x''' END || ')::' || format_type(a.atttypid, a.atttypmod), \
                            ', ' ORDER BY a.attnum) \
                   INTO cols, vals \
                 FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid \
                 WHERE a.attrelid = '{table}'::regclass AND a.attnum > 0 AND NOT a.attisdropped \
                   AND (a.attname IN ('timestamp', 'id') OR (a.attnotnull AND NOT a.atthasdef)); \
                 EXECUTE format('INSERT INTO {table} (%s) SELECT %s \
                     FROM generate_series({first}, {last}) AS d, \
                          generate_series(0, {partitions} - 1) AS p', cols, vals); \
                 END $$"
            ))
            .await
            .map_err(|error| anyhow::anyhow!("filling chunks of {table}: {error}"))?;
            first = last + 1;
        }
        Ok(())
    }

    async fn count(db: &DbConnection, sql: &str) -> anyhow::Result<i64> {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql.to_owned(),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("{sql}: no row"))?;
        Ok(row.try_get::<i64>("", "n")?)
    }

    /// Foreign keys from the telemetry tables, or any of their chunks, to the
    /// detached parents.
    async fn telemetry_foreign_keys_anywhere(db: &DbConnection) -> anyhow::Result<i64> {
        count(
            db,
            "SELECT count(*) AS n FROM pg_constraint c \
             WHERE c.contype = 'f' \
               AND c.confrelid IN (to_regclass('projects'), to_regclass('environments'), \
                                   to_regclass('deployments'), to_regclass('error_groups')) \
               AND (c.conrelid IN (to_regclass('proxy_logs'), to_regclass('events'), \
                                   to_regclass('error_events'), to_regclass('request_logs'), \
                                   to_regclass('performance_metrics')) \
                    OR c.conrelid IN (SELECT inhrelid FROM pg_inherits \
                        WHERE inhparent IN (to_regclass('proxy_logs'), to_regclass('events'), \
                                            to_regclass('error_events'))))",
        )
        .await
    }

    /// The reported failure, reproduced: with enough chunks for the lock
    /// table, the old migration runs out of lock slots in its one transaction,
    /// while the new migration plus the post-migration step, one constraint
    /// and one chunk per transaction, drop every telemetry foreign key. Both
    /// are no-ops when run again.
    #[tokio::test]
    async fn test_project_data_purges_fits_a_small_lock_table() -> anyhow::Result<()> {
        use sea_orm::TransactionTrait;

        let Some((container, db)) = start_db(&[
            "-c",
            "max_locks_per_transaction=32",
            "-c",
            "max_connections=20",
        ])
        .await?
        else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        migrate_up_to_project_data_purges(db.as_ref()).await?;
        // A chunk per day and space partition on the two big hypertables.
        fill_chunks(db.as_ref(), "events", 60, 4).await?;
        fill_chunks(db.as_ref(), "proxy_logs", 60, 4).await?;
        let total_chunks = count(
            db.as_ref(),
            "SELECT count(*) AS n FROM timescaledb_information.chunks",
        )
        .await?;
        let events_chunks = count(
            db.as_ref(),
            "SELECT count(*) AS n FROM timescaledb_information.chunks \
             WHERE hypertable_name = 'events'",
        )
        .await?;
        assert!(
            total_chunks >= 300,
            "only {total_chunks} chunks were created"
        );

        // The old migration, in the transaction SeaORM gives a migration.
        let txn = db.begin().await?;
        let old = txn
            .execute_unprepared(&old_project_data_purges_drop_sql())
            .await;
        txn.rollback().await?;
        let old_error = match old {
            Ok(_) => anyhow::bail!(
                "the old migration was expected to run out of lock slots with {total_chunks} chunks"
            ),
            Err(error) => error,
        };
        assert!(
            lock_capacity::is_lock_exhaustion(&old_error),
            "expected SQLSTATE 53200 from the lock table, got: {old_error}"
        );

        // That real error, as the migration runners report it.
        let exhausted = migrations::lock_exhaustion(
            db.as_ref(),
            lock_capacity::SchemaStep::Migration(PROJECT_DATA_PURGES.to_string()),
            &old_error,
        )
        .await;
        let message = exhausted.to_string();
        assert!(message.contains(PROJECT_DATA_PURGES), "{message}");
        assert!(
            matches!(
                &exhausted.table,
                lock_capacity::LockedTable::LargestHypertable(table)
                    if table == "events" || table == "proxy_logs"
            ),
            "{message}"
        );
        assert!(
            message.contains(&format!("{total_chunks} TimescaleDB chunk(s)")),
            "{message}"
        );
        let suggested = exhausted
            .suggested_max_locks_per_transaction()
            .ok_or_else(|| anyhow::anyhow!("no suggestion: {message}"))?;
        assert!(suggested > 16, "{message}");
        assert!(
            message.contains(&format!(
                "max_locks_per_transaction to at least {suggested}"
            )),
            "{message}"
        );
        assert!(message.contains("restart PostgreSQL"), "{message}");
        let as_service = temps_core::ServiceError::from(SchemaError::from(exhausted));
        assert!(matches!(
            as_service,
            temps_core::ServiceError::Configuration { .. }
        ));

        // The new migration, then the post-migration step.
        run_migrations(db.as_ref()).await?;
        assert!(
            !remaining_telemetry_foreign_keys(db.get_postgres_connection_pool())
                .await?
                .is_empty(),
            "the migration must leave the foreign keys to the post-migration step"
        );
        let events = std::sync::Mutex::new(Vec::new());
        let outcome = run_post_migration_indexes_streaming(db.as_ref(), |progress| {
            if let MaintenanceProgress::ForeignKeyDetachFinished {
                table,
                chunks,
                per_chunk,
                ..
            } = progress
            {
                if let Ok(mut events) = events.lock() {
                    events.push((table.to_string(), chunks, per_chunk));
                }
            }
        })
        .await;
        if outcome.is_err() {
            print_server_log(&container).await;
        }
        outcome?;
        assert_eq!(telemetry_foreign_keys_anywhere(db.as_ref()).await?, 0);
        let events = events
            .into_inner()
            .map_err(|_| anyhow::anyhow!("progress lock poisoned"))?;
        assert!(
            events
                .iter()
                .any(|(table, chunks, per_chunk)| table == "events"
                    && *chunks as i64 == events_chunks
                    && *per_chunk),
            "events must be detached chunk by chunk ({events_chunks} chunks): {events:?}"
        );

        // Running both again changes nothing.
        run_migrations(db.as_ref()).await?;
        run_post_migration_indexes(db.as_ref()).await?;
        detach_telemetry_foreign_keys(db.as_ref()).await?;
        assert_eq!(telemetry_foreign_keys_anywhere(db.as_ref()).await?, 0);
        assert!(get_pending_migration_names(db.as_ref()).await?.is_empty());
        Ok(())
    }

    /// An instance that applied the earlier `m20261105_000001_project_data_purges`,
    /// which dropped the foreign keys itself, has none left: the
    /// post-migration step does nothing there and does not fail.
    #[tokio::test]
    async fn test_detach_after_the_old_migration_is_a_no_op() -> anyhow::Result<()> {
        use sea_orm::TransactionTrait;

        let Some((_container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        migrate_up_to_project_data_purges(db.as_ref()).await?;
        fill_chunks(db.as_ref(), "events", 3, 4).await?;

        // The old migration on a small history: it fit, and it is recorded.
        let txn = db.begin().await?;
        txn.execute_unprepared(&old_project_data_purges_drop_sql())
            .await?;
        txn.commit().await?;
        Migrator::up(db.as_ref(), Some(1)).await?;
        assert_eq!(telemetry_foreign_keys_anywhere(db.as_ref()).await?, 0);

        // The new binary: the rest of the migrations and maintenance, twice.
        for _ in 0..2 {
            run_migrations(db.as_ref()).await?;
            let detached = std::sync::atomic::AtomicUsize::new(0);
            run_post_migration_indexes_streaming(db.as_ref(), |progress| {
                if matches!(
                    progress,
                    MaintenanceProgress::ForeignKeyDetachStarted { .. }
                ) {
                    detached.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .await?;
            assert_eq!(detached.into_inner(), 0, "nothing was left to drop");
        }
        assert_eq!(
            count(db.as_ref(), "SELECT count(*) AS n FROM project_data_purges").await?,
            0
        );
        Ok(())
    }

    /// Retention (`drop_chunks`) can remove a chunk after the step listed the
    /// copies of a foreign key on the chunks and before it drops them: the
    /// chunk that is gone is skipped, and the step still finishes.
    #[tokio::test]
    async fn test_detach_skips_a_chunk_dropped_by_retention() -> anyhow::Result<()> {
        let Some((_container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        run_migrations(db.as_ref()).await?;
        fill_chunks(db.as_ref(), "events", 6, 1).await?;

        let mut connection = db.get_postgres_connection_pool().acquire().await?;
        let (oid, foreign_key) = maintenance::list_telemetry_foreign_keys(&mut connection)
            .await?
            .into_iter()
            .find(|(_, foreign_key)| foreign_key.table == "events")
            .ok_or_else(|| anyhow::anyhow!("events has no telemetry foreign key"))?;
        let chunks: Vec<(String, String)> = sqlx::query_as(maintenance::CHUNK_FOREIGN_KEYS_SQL)
            .bind(oid)
            .fetch_all(&mut *connection)
            .await?;
        assert!(chunks.len() >= 6, "events has {} chunk(s)", chunks.len());

        // Retention runs between the listing and the drops.
        let dropped = count(
            db.as_ref(),
            "SELECT count(*) AS n FROM drop_chunks('events', older_than => now() - interval '3 days')",
        )
        .await?;
        assert!(dropped > 0, "retention dropped no chunk of events");
        let mut gone = Vec::new();
        for (chunk, chunk_constraint) in &chunks {
            let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
                .bind(chunk)
                .fetch_one(&mut *connection)
                .await?;
            if !exists {
                gone.push((chunk, chunk_constraint));
            }
        }
        let (gone_chunk, gone_constraint) = gone
            .first()
            .ok_or_else(|| anyhow::anyhow!("every listed chunk of events still exists"))?;
        let unguarded = sqlx::query(&format!(
            "ALTER TABLE {gone_chunk} DROP CONSTRAINT IF EXISTS {gone_constraint}"
        ))
        .execute(&mut *connection)
        .await;
        assert!(
            unguarded.is_err(),
            "dropping a constraint from the removed chunk {gone_chunk} without IF EXISTS \
             on the table must fail"
        );

        match maintenance::drop_chunk_copies(&mut connection, &foreign_key, &chunks).await {
            Ok(_) => {}
            Err(maintenance::DetachFailure::LockSlotsExhausted(error)) => {
                anyhow::bail!("{error}")
            }
            Err(maintenance::DetachFailure::Other(message)) => anyhow::bail!("{message}"),
        }
        drop(connection);
        detach_telemetry_foreign_keys(db.as_ref()).await?;
        assert_eq!(telemetry_foreign_keys_anywhere(db.as_ref()).await?, 0);
        Ok(())
    }

    /// The telemetry tables are looked up in the current schema only. A
    /// session whose schema has none of them, with a migrated `public` after
    /// it on the `search_path`, finds no foreign key to drop and leaves
    /// `public` alone.
    #[tokio::test]
    async fn test_detach_ignores_telemetry_tables_outside_the_current_schema() -> anyhow::Result<()>
    {
        let Some((container, db)) = start_fresh_db().await? else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        run_migrations(db.as_ref()).await?;
        let in_public = remaining_telemetry_foreign_keys(db.get_postgres_connection_pool())
            .await?
            .len();
        assert!(
            in_public > 0,
            "the migrations leave the foreign keys in place"
        );

        db.execute_unprepared("CREATE SCHEMA scratch").await?;
        let port = container.get_host_port_ipv4(5432).await?;
        let scratch_url = format!(
            "postgresql://postgres:postgres@localhost:{port}/postgres\
             ?options=-c%20search_path%3Dscratch%2Cpublic"
        );
        let scratch = retry_db_connect(|| connect_without_migrations(&scratch_url)).await?;
        assert_eq!(
            count(
                scratch.as_ref(),
                "SELECT count(*) AS n FROM (SELECT 1 WHERE current_schema() = 'scratch') s"
            )
            .await?,
            1,
            "the session must resolve names in scratch first"
        );

        assert!(
            remaining_telemetry_foreign_keys(scratch.get_postgres_connection_pool())
                .await?
                .is_empty()
        );
        detach_telemetry_foreign_keys(scratch.as_ref()).await?;
        assert_eq!(
            remaining_telemetry_foreign_keys(db.get_postgres_connection_pool())
                .await?
                .len(),
            in_public,
            "public keeps its foreign keys"
        );
        Ok(())
    }

    async fn start_db(
        args: &[&str],
    ) -> anyhow::Result<
        Option<(
            testcontainers::ContainerAsync<GenericImage>,
            std::sync::Arc<DbConnection>,
        )>,
    > {
        let mut cmd = vec!["postgres".to_string()];
        cmd.extend(args.iter().map(|arg| arg.to_string()));
        let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .with_cmd(cmd)
            .with_startup_timeout(std::time::Duration::from_secs(120))
            .start()
            .await
        {
            Ok(c) => c,
            Err(error)
                if temps_test_utils::is_container_runtime_unavailable(&error.to_string()) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error.into()),
        };

        let port = container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        let db = retry_db_connect(|| connect_without_migrations(&database_url)).await?;

        Ok(Some((container, db)))
    }

    async fn start_fresh_db() -> anyhow::Result<
        Option<(
            testcontainers::ContainerAsync<GenericImage>,
            std::sync::Arc<DbConnection>,
        )>,
    > {
        let container = match GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await
        {
            Ok(c) => c,
            Err(_) => return Ok(None), // Docker not available
        };

        let port = container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        let db = retry_db_connect(|| connect_without_migrations(&database_url)).await?;

        Ok(Some((container, db)))
    }
}
