//! Database connection and query utilities

pub use sea_orm;
pub mod approx_count;
mod connection;

pub use approx_count::{approximate_row_count, count_for_pagination, CountKind};
pub use connection::{
    connect_without_migrations, establish_connection, get_pending_migration_names, run_migrations,
    run_migrations_reported, run_migrations_streaming, run_post_migration_backfill, DbConnection,
    MigrationProgress, MigrationRunReport, MigrationStepResult,
};

// Export test utilities for use by other crates in their tests
pub mod test_utils;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn test_establish_connection() -> anyhow::Result<()> {
        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await?;

        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        // Wait a bit for the database to be ready, then connect with retries
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        let mut retries = 5;
        let db = loop {
            match Database::connect(&database_url).await {
                Ok(db) => break db,
                Err(e) if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    if retries == 0 {
                        return Err(anyhow::anyhow!(
                            "Failed to connect to database after retries: {}",
                            e
                        ));
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("Failed to connect to database: {}", e)),
            }
        };

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
        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg18")
            .with_env_var("POSTGRES_DB", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await?;

        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!("postgresql://postgres:postgres@localhost:{}/postgres", port);

        // Wait a bit for the database to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Retry connection setup
        let mut retries = 5;
        let _connection = loop {
            match establish_connection(&database_url).await {
                Ok(conn) => break conn,
                Err(e) if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    if retries == 0 {
                        return Err(anyhow::anyhow!(
                            "Failed to establish connection after retries: {}",
                            e
                        ));
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("Failed to establish connection: {}", e)),
            }
        };

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

        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        let mut retries = 5;
        let db = loop {
            match connect_without_migrations(&database_url).await {
                Ok(db) => break db,
                Err(e) if retries > 0 => {
                    retries -= 1;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    if retries == 0 {
                        return Err(anyhow::anyhow!("connect failed after retries: {e}"));
                    }
                }
                Err(e) => return Err(anyhow::anyhow!("connect failed: {e}")),
            }
        };

        Ok(Some((container, db)))
    }
}
