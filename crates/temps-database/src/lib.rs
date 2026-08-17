//! Database connection and query utilities

pub use sea_orm;
pub mod approx_count;
mod connection;

pub use approx_count::{approximate_row_count, count_for_pagination, CountKind};
pub use connection::{
    cancel_migration_backend, connect_for_migrate, connect_without_migrations,
    establish_connection, get_pending_migration_names, run_migrations, run_migrations_reported,
    run_migrations_streaming, run_post_migration_backfill, run_post_migration_backfill_streaming,
    run_post_migration_indexes, run_post_migration_indexes_streaming, DbConnection,
    MaintenanceProgress, MigrationProgress, MigrationRunReport, MigrationStepResult,
};

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
        run_post_migration_indexes(connection.as_ref()).await?;
        run_post_migration_indexes(connection.as_ref()).await?;
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

    /// Start a throwaway TimescaleDB container and connect (no auto-migrate).
    /// Returns `None` when Docker is unavailable so callers skip gracefully.
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
