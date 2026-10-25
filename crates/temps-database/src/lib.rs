// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Database connection and query utilities

pub use sea_orm;
pub mod approx_count;
mod connection;
pub mod schema_guard;

pub use approx_count::{approximate_row_count, count_for_pagination, CountKind};
pub use connection::{
    cancel_migration_backend, connect_for_migrate, connect_options, connect_without_migrations,
    defined_migration_count, ensure_no_unprotected_upgrade, establish_connection,
    get_pending_migration_names, reconcile_otel_trace_summaries, retry_database_startup,
    run_migrations, run_migrations_reported, run_migrations_streaming, run_post_migration_backfill,
    run_post_migration_backfill_streaming, run_post_migration_indexes,
    run_post_migration_indexes_streaming, wait_for_database, DbConnection, MaintenanceProgress,
    MigrationProgress, MigrationRunReport, MigrationStepResult, StartupAttemptFailure,
    DEFAULT_DATABASE_STARTUP_WAIT,
};
pub use schema_guard::{check_schema_compatibility, SchemaGuardError, SchemaStatus};

// Export test utilities for use by other crates in their tests
pub mod test_utils;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use sea_orm_migration::MigratorTrait;
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
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
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
                if crate::test_utils::is_container_runtime_unavailable(&error.to_string()) =>
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
