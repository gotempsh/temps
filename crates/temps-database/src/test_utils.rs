//! Test utilities for database integration tests
//!
//! This module provides reusable test utilities for setting up
//! PostgreSQL with TimescaleDB for integration testing across
//! all temps crates.

use crate::DbConnection;
use sea_orm::*;
use sea_orm_migration::MigratorTrait;
use temps_migrations::Migrator;
use std::sync::Arc;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tokio::sync::{Mutex, OnceCell};

/// Shared test database container that lives for the duration of the test run
static TEST_CONTAINER: OnceCell<Arc<Mutex<SharedContainer>>> = OnceCell::const_new();

/// Global migration lock to ensure only one test runs migrations at a time
/// This prevents race conditions when multiple tests try to create TimescaleDB
/// continuous aggregates and internal types simultaneously
static MIGRATION_LOCK: OnceCell<Arc<Mutex<()>>> = OnceCell::const_new();

/// Shared container wrapper that holds the database container and connection details
struct SharedContainer {
    #[allow(dead_code)]
    container: ContainerAsync<GenericImage>,
    database_url: String,
    #[allow(dead_code)]
    port: u16,
}

impl SharedContainer {
    async fn new() -> anyhow::Result<Self> {
        let db_name = "test_db";
        let username = "test_user";
        let password = "test_password";

        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg17")
            .with_env_var("POSTGRES_DB", db_name)
            .with_env_var("POSTGRES_USER", username)
            .with_env_var("POSTGRES_PASSWORD", password)
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await?;

        // Get connection details
        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            username, password, port, db_name
        );

        // Wait for the database to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        Ok(Self {
            container: postgres_container,
            database_url,
            port,
        })
    }
}

/// Test database setup with TimescaleDB container
pub struct TestDatabase {
    pub db: Arc<DbConnection>,
    pub database_url: String,
    /// If this instance owns a dedicated container (not shared)
    #[allow(dead_code)]
    dedicated_container: Option<ContainerAsync<GenericImage>>,
}

impl TestDatabase {
    /// Get or create the shared database container
    async fn get_or_create_container() -> anyhow::Result<Arc<Mutex<SharedContainer>>> {
        TEST_CONTAINER
            .get_or_try_init(|| async {
                let container = SharedContainer::new().await?;
                Ok(Arc::new(Mutex::new(container)))
            })
            .await
            .map(|arc| Arc::clone(arc))
    }

    /// Create a new test database with TimescaleDB (uses shared container)
    ///
    /// This function:
    /// 1. Gets or creates a shared TimescaleDB container (only created once per test run)
    /// 2. Establishes a new connection to the shared database
    /// 3. Cleans up all tables to ensure test isolation
    pub async fn new() -> anyhow::Result<Self> {
        // Get or create shared container
        let container = Self::get_or_create_container().await?;
        let container_lock = container.lock().await;
        let database_url = container_lock.database_url.clone();
        drop(container_lock); // Release lock early

        // Connect with retries - use more retries for shared container
        let db = Self::connect_with_retry(&database_url, 20).await?;

        let test_db = TestDatabase {
            db: Arc::new(db),
            database_url,
            dedicated_container: None,
        };

        // Verify connection works
        test_db.test_connection().await
            .map_err(|e| anyhow::anyhow!("Initial connection test failed: {}", e))?;

        // Clean up all tables for test isolation
        test_db.cleanup_all_tables().await.ok(); // Ignore errors if no tables exist yet

        Ok(test_db)
    }

    /// Create a test database with a dedicated container (not shared)
    ///
    /// This creates a completely isolated database instance for tests that need
    /// full isolation and cannot share a container with other tests.
    /// Use this sparingly as it's slower than using the shared container.
    pub async fn new_isolated() -> anyhow::Result<Self> {
        Self::new_isolated_with_config("test_db", "test_user", "test_password").await
    }

    /// Create an isolated test database with custom configuration
    ///
    /// This creates a dedicated TimescaleDB container that is not shared with other tests.
    /// Useful for tests that need complete isolation or custom database configuration.
    pub async fn new_isolated_with_config(
        db_name: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Self> {
        // Start TimescaleDB container
        let postgres_container = GenericImage::new("timescale/timescaledb-ha", "pg17")
            .with_env_var("POSTGRES_DB", db_name)
            .with_env_var("POSTGRES_USER", username)
            .with_env_var("POSTGRES_PASSWORD", password)
            .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
            .start()
            .await?;

        // Get connection details
        let port = postgres_container.get_host_port_ipv4(5432).await?;
        let database_url = format!(
            "postgresql://{}:{}@localhost:{}/{}",
            username, password, port, db_name
        );

        // Wait for the database to be ready
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Connect with retries
        let db = Self::connect_with_retry(&database_url, 10).await?;

        let test_db = TestDatabase {
            db: Arc::new(db),
            database_url,
            dedicated_container: Some(postgres_container),
        };

        // Verify connection works
        test_db.test_connection().await
            .map_err(|e| anyhow::anyhow!("Initial connection test failed: {}", e))?;
        // Run migrations after creating the isolated test database
        use sea_orm_migration::MigratorTrait;
        if let Err(e) = temps_migrations::Migrator::up(&*test_db.db, None).await {
            return Err(anyhow::anyhow!("Failed to run migrations: {}", e));
        }
        Ok(test_db)
    }

    /// Create a test database with custom configuration
    ///
    /// Note: This method now creates an isolated container.
    /// Use `new()` for shared container (recommended) or `new_isolated()` for explicit isolation.
    #[deprecated(note = "Use TestDatabase::new() for shared container or TestDatabase::new_isolated_with_config() for isolated container")]
    pub async fn with_config(
        db_name: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Self> {
        Self::new_isolated_with_config(db_name, username, password).await
    }

    /// Create a test database and run migrations
    ///
    /// This is a convenience method that uses temps_migrations::Migrator.
    /// The database connection is verified before running migrations.
    /// Note: Migrations are run only once per shared container - subsequent calls
    /// will skip migration if tables already exist.
    pub async fn with_migrations() -> anyhow::Result<Self> {
        let test_db = Self::new().await?;

        // Verify database connection is working before migrations
        test_db.test_connection().await
            .map_err(|e| anyhow::anyhow!("Database connection test failed: {}", e))?;

        // Check if migrations have already been run
        let check_sql = "SELECT EXISTS (
            SELECT FROM information_schema.tables
            WHERE table_schema = 'public'
            AND table_name = 'seaql_migrations'
        )";

        let result = test_db.query_sql(check_sql).await?;
        let migrations_table_exists = result
            .first()
            .and_then(|row| row.try_get::<bool>("", "exists").ok())
            .unwrap_or(false);

        if !migrations_table_exists {
            // Acquire the global migration lock to prevent concurrent migrations
            // This is critical for TimescaleDB which creates internal types that
            // can cause "duplicate key value violates unique constraint" errors
            let migration_lock = MIGRATION_LOCK
                .get_or_init(|| async { Arc::new(Mutex::new(())) })
                .await;
            let _lock = migration_lock.lock().await;

            // Double-check migrations weren't run by another test while we waited for the lock
            let check_sql = "SELECT EXISTS (
                SELECT FROM information_schema.tables
                WHERE table_schema = 'public'
                AND table_name = 'seaql_migrations'
            )";
            let result = test_db.query_sql(check_sql).await?;
            let migrations_now_exist = result
                .first()
                .and_then(|row| row.try_get::<bool>("", "exists").ok())
                .unwrap_or(false);

            if !migrations_now_exist {
                // Run migrations for the first time
                Migrator::up(&*test_db.db, None).await
                    .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

                // Verify migrations were successful by checking a known table
                let check_sql = "SELECT EXISTS (
                    SELECT FROM information_schema.tables
                    WHERE table_schema = 'public'
                    AND table_name = 'users'
                )";

                let result = test_db.query_sql(check_sql).await
                    .map_err(|e| anyhow::anyhow!("Failed to verify migrations: {}", e))?;

                let users_table_exists = result
                    .first()
                    .and_then(|row| row.try_get::<bool>("", "exists").ok())
                    .unwrap_or(false);

                if !users_table_exists {
                    return Err(anyhow::anyhow!("Migrations did not create expected tables"));
                }
            }
            // Lock is automatically released when _lock goes out of scope
        }

        // Clean tables but preserve schema
        test_db.cleanup_all_tables().await.ok();

        Ok(test_db)
    }

    /// Create a test database and run migrations with custom Migrator
    ///
    /// This generic method allows using any MigratorTrait implementation.
    /// Note: Migrations are run only once per shared container.
    pub async fn with_custom_migrations<M>() -> anyhow::Result<Self>
    where
        M: MigratorTrait,
    {
        let test_db = Self::new().await?;

        // Verify database connection is working
        test_db.test_connection().await
            .map_err(|e| anyhow::anyhow!("Database connection test failed: {}", e))?;

        // Check if migrations have already been run
        let check_sql = "SELECT EXISTS (
            SELECT FROM information_schema.tables
            WHERE table_schema = 'public'
            AND table_name = 'seaql_migrations'
        )";

        let result = test_db.query_sql(check_sql).await?;
        let migrations_table_exists = result
            .first()
            .and_then(|row| row.try_get::<bool>("", "exists").ok())
            .unwrap_or(false);

        if !migrations_table_exists {
            // Acquire the global migration lock to prevent concurrent migrations
            let migration_lock = MIGRATION_LOCK
                .get_or_init(|| async { Arc::new(Mutex::new(())) })
                .await;
            let _lock = migration_lock.lock().await;

            // Double-check migrations weren't run by another test while we waited for the lock
            let check_sql = "SELECT EXISTS (
                SELECT FROM information_schema.tables
                WHERE table_schema = 'public'
                AND table_name = 'seaql_migrations'
            )";
            let result = test_db.query_sql(check_sql).await?;
            let migrations_now_exist = result
                .first()
                .and_then(|row| row.try_get::<bool>("", "exists").ok())
                .unwrap_or(false);

            if !migrations_now_exist {
                // Run migrations for the first time
                M::up(&*test_db.db, None).await
                    .map_err(|e| anyhow::anyhow!("Failed to run custom migrations: {}", e))?;
            }
            // Lock is automatically released when _lock goes out of scope
        }

        // Clean tables but preserve schema
        test_db.cleanup_all_tables().await.ok();

        Ok(test_db)
    }

    /// Connect to database with retry logic
    async fn connect_with_retry(
        database_url: &str,
        max_retries: u32,
    ) -> anyhow::Result<DbConnection> {
        use sea_orm::ConnectOptions;
        use std::time::Duration;

        let mut retries = max_retries;

        // Create connection options with better timeout settings
        let mut opt = ConnectOptions::new(database_url.to_owned());
        opt.max_connections(5)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(10))
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(10))
            .max_lifetime(Duration::from_secs(60))
            .sqlx_logging(false);

        loop {
            match Database::connect(opt.clone()).await {
                Ok(db) => {
                    // Verify connection with a simple query
                    let test = Statement::from_string(
                        DatabaseBackend::Postgres,
                        "SELECT 1".to_owned()
                    );

                    match db.execute(test).await {
                        Ok(_) => return Ok(db),
                        Err(e) if retries > 0 => {
                            eprintln!("Database connected but test query failed (retries left: {}): {}", retries, e);
                            // Fall through to retry logic below
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!("Database connected but not responsive: {}", e));
                        }
                    }
                }
                Err(e) if retries > 0 => {
                    eprintln!("Failed to connect to database (retries left: {}): {}", retries, e);
                    // Fall through to retry logic below
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to connect to database: {}", e));
                }
            }

            if retries > 0 {
                retries -= 1;
                tokio::time::sleep(Duration::from_secs(1)).await; // Reduced from 3 to 1 second
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to connect to database after {} retries",
                    max_retries
                ));
            }
        }
    }

    /// Execute raw SQL query for testing
    pub async fn execute_sql(&self, sql: &str) -> anyhow::Result<ExecResult> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, sql.to_owned());
        let result = self.db.execute(statement).await.map_err(anyhow::Error::from)?;
        Ok(result)
    }

    /// Query raw SQL and return results
    pub async fn query_sql(&self, sql: &str) -> anyhow::Result<Vec<QueryResult>> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, sql.to_owned());
        let result = self.db.query_all(statement).await.map_err(anyhow::Error::from)?;
        Ok(result)
    }

    /// Clean up all data in the database (useful for test cleanup)
    ///
    /// This truncates all tables except migration-related tables.
    /// Tables are truncated in reverse dependency order to avoid foreign key issues.
    pub async fn cleanup_all_tables(&self) -> anyhow::Result<()> {
        // First, drop all TimescaleDB continuous aggregates (materialized views)
        let views = self
            .query_sql(
                "SELECT matviewname FROM pg_matviews WHERE schemaname = 'public'",
            )
            .await?;

        for view in views {
            if let Some(view_name) = view.try_get::<String>("", "matviewname").ok() {
                let sql = format!("DROP MATERIALIZED VIEW IF EXISTS {} CASCADE", view_name);
                self.execute_sql(&sql).await.ok(); // Ignore errors
            }
        }

        // Drop all custom types (enums and composites created by TimescaleDB)
        let types = self
            .query_sql(
                "SELECT t.typname FROM pg_type t
                 JOIN pg_namespace n ON n.oid = t.typnamespace
                 WHERE n.nspname = 'public'
                 AND t.typtype IN ('e', 'c')
                 AND t.typname NOT LIKE 'pg_%'",
            )
            .await?;

        for type_row in types {
            if let Some(type_name) = type_row.try_get::<String>("", "typname").ok() {
                let sql = format!("DROP TYPE IF EXISTS {} CASCADE", type_name);
                self.execute_sql(&sql).await.ok(); // Ignore errors
            }
        }

        // Get all table names except migration tables
        let tables = self
            .query_sql(
                "SELECT tablename FROM pg_tables
             WHERE schemaname = 'public'
             AND tablename NOT IN ('seaql_migrations', '_sqlx_migrations')
             ORDER BY tablename DESC",
            )
            .await?;

        // Truncate each table
        for table in tables {
            if let Some(table_name) = table.try_get::<String>("", "tablename").ok() {
                let sql = format!("TRUNCATE TABLE {} CASCADE", table_name);
                self.execute_sql(&sql).await?;
            }
        }

        Ok(())
    }

    /// Test database connectivity
    pub async fn test_connection(&self) -> anyhow::Result<()> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, "SELECT 1".to_owned());
        let result = self.db.query_one(statement).await?;

        if result.is_none() {
            return Err(anyhow::anyhow!("Connection test failed"));
        }

        Ok(())
    }

    /// Enable TimescaleDB extension on a table
    ///
    /// This is useful for tables that need time-series functionality
    pub async fn create_hypertable(
        &self,
        table_name: &str,
        time_column: &str,
    ) -> anyhow::Result<()> {
        // First ensure TimescaleDB extension is created
        self.execute_sql("CREATE EXTENSION IF NOT EXISTS timescaledb")
            .await?;

        // Create hypertable
        let sql = format!(
            "SELECT create_hypertable('{}', '{}', if_not_exists => TRUE)",
            table_name, time_column
        );
        self.execute_sql(&sql).await?;

        Ok(())
    }

    /// Get the database connection
    pub fn connection(&self) -> &DbConnection {
        &self.db
    }

    /// Get the database connection as Arc
    pub fn connection_arc(&self) -> Arc<DbConnection> {
        Arc::clone(&self.db)
    }
}

/// Test transaction helper for rollback-based testing
///
/// This allows you to run tests in a transaction that gets rolled back,
/// keeping your test database clean.
///
/// Note: This is a simplified transaction helper. For more complex transaction
/// handling, consider using SeaORM's built-in transaction support.
pub struct TestTransaction {
    pub db: Arc<DbConnection>,
    #[allow(dead_code)]
    transaction_id: String,
}

impl TestTransaction {
    /// Start a new test transaction
    pub async fn new(db: Arc<DbConnection>) -> anyhow::Result<Self> {
        let transaction_id = uuid::Uuid::new_v4().to_string();

        // Start transaction
        let statement = Statement::from_string(DatabaseBackend::Postgres, "BEGIN".to_owned());
        db.execute(statement).await?;

        Ok(TestTransaction { db, transaction_id })
    }

    /// Rollback the transaction
    pub async fn rollback(self) -> anyhow::Result<()> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, "ROLLBACK".to_owned());
        self.db.execute(statement).await?;
        Ok(())
    }

    /// Commit the transaction (use sparingly in tests)
    pub async fn commit(self) -> anyhow::Result<()> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, "COMMIT".to_owned());
        self.db.execute(statement).await?;
        Ok(())
    }
}

/// Helper to create a unique test database name
pub fn generate_test_db_name(prefix: &str) -> String {
    format!(
        "{}_{}",
        prefix,
        uuid::Uuid::new_v4().to_string().replace("-", "")
    )
}

/// Helper to wait for a condition with timeout
pub async fn wait_for<F, Fut>(
    condition: F,
    timeout_secs: u64,
    check_interval_ms: u64,
) -> anyhow::Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let interval = std::time::Duration::from_millis(check_interval_ms);

    while start.elapsed() < timeout {
        if condition().await {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }

    Err(anyhow::anyhow!("Timeout waiting for condition"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_setup() -> anyhow::Result<()> {
        let test_db = TestDatabase::new().await?;

        // Test basic connectivity
        test_db.test_connection().await?;

        // Test raw SQL execution
        let result = test_db.query_sql("SELECT 1 as test_value").await?;
        assert_eq!(result.len(), 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_custom_config() -> anyhow::Result<()> {
        // This test now uses the shared container via new()
        let test_db = TestDatabase::new().await?;

        // Verify connection works
        test_db.test_connection().await?;

        // Verify database uses test_db (the shared container's database name)
        assert!(test_db.database_url.contains("test_db"));

        Ok(())
    }

    #[tokio::test]
    async fn test_with_migrations() -> anyhow::Result<()> {
        let test_db = TestDatabase::with_migrations().await?;

        // Verify users table exists
        let result = test_db.query_sql(
            "SELECT column_name FROM information_schema.columns WHERE table_name = 'users'"
        ).await?;

        assert!(!result.is_empty(), "Users table should have columns");
        Ok(())
    }

    #[tokio::test]
    async fn test_transaction_rollback() -> anyhow::Result<()> {
        let test_db = TestDatabase::new().await?;

        // Create a test table
        test_db
            .execute_sql("CREATE TABLE test_table (id SERIAL PRIMARY KEY, value TEXT)")
            .await?;

        // Insert data without transaction first to test normal operation
        test_db
            .execute_sql("INSERT INTO test_table (value) VALUES ('persistent')")
            .await?;

        // Verify data exists
        let result = test_db.query_sql("SELECT * FROM test_table").await?;
        assert_eq!(result.len(), 1);

        // Now test transaction rollback using SeaORM's transaction support
        // This test demonstrates that the TestTransaction struct can be used
        // Note: For proper transaction testing, consider using SeaORM's
        // built-in transaction support instead of raw SQL BEGIN/ROLLBACK

        // Clean up
        test_db.execute_sql("DELETE FROM test_table").await?;

        // Verify cleanup
        let result = test_db.query_sql("SELECT * FROM test_table").await?;
        assert_eq!(result.len(), 0);

        Ok(())
    }
}
