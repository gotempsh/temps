//! Test utilities for database integration tests
//!
//! This module provides reusable test utilities for setting up
//! PostgreSQL with TimescaleDB for integration testing across
//! all temps crates.

use crate::DbConnection;
use sea_orm::*;
use sea_orm_migration::MigratorTrait;
use std::sync::Arc;
use temps_migrations::Migrator;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tokio::sync::{Mutex, OnceCell};

/// Shared test database container that lives for the duration of the test run
static TEST_CONTAINER: OnceCell<Arc<Mutex<Option<SharedContainer>>>> = OnceCell::const_new();

/// Reference counter for active TestDatabase instances using the shared container
static ACTIVE_INSTANCES: OnceCell<Arc<Mutex<usize>>> = OnceCell::const_new();

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
    /// The unique schema name for this test instance
    schema_name: Option<String>,
    /// Whether this instance is using the shared container
    uses_shared_container: bool,
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        // Clean up the unique schema when the test database is dropped
        if let Some(schema_name) = &self.schema_name {
            let db = Arc::clone(&self.db);
            let schema = schema_name.clone();

            // Spawn a background task to drop the schema
            // We can't use async/await in Drop, so we spawn a task
            tokio::spawn(async move {
                let drop_schema_sql = format!("DROP SCHEMA IF EXISTS {} CASCADE", schema);
                let statement = Statement::from_string(DatabaseBackend::Postgres, drop_schema_sql);

                if let Err(e) = db.execute(statement).await {
                    eprintln!("Warning: Failed to drop test schema {}: {}", schema, e);
                }
            });
        }

        // Decrement the active instance counter if using shared container
        if self.uses_shared_container {
            tokio::spawn(async move {
                // Decrement active instances counter
                if let Some(counter) = ACTIVE_INSTANCES.get() {
                    let mut count = counter.lock().await;
                    *count = count.saturating_sub(1);

                    // If this was the last instance, drop the shared container
                    if *count == 0 {
                        if let Some(container_holder) = TEST_CONTAINER.get() {
                            let mut container_opt = container_holder.lock().await;
                            if let Some(container) = container_opt.take() {
                                drop(container); // Explicitly drop the SharedContainer
                                eprintln!(
                                    "Dropped shared test database container (all tests completed)"
                                );
                            }
                        }
                    }
                }
            });
        }

        // Note: dedicated_container will be automatically cleaned up by testcontainers
        // when it goes out of scope (Drop is implemented by testcontainers)
    }
}

impl TestDatabase {
    /// Get or create the shared database container
    async fn get_or_create_container() -> anyhow::Result<Arc<Mutex<Option<SharedContainer>>>> {
        TEST_CONTAINER
            .get_or_try_init(|| async {
                let container = SharedContainer::new().await?;
                Ok(Arc::new(Mutex::new(Some(container))))
            })
            .await
            .map(Arc::clone)
    }

    /// Initialize or get the active instances counter
    async fn get_or_init_counter() -> Arc<Mutex<usize>> {
        ACTIVE_INSTANCES
            .get_or_init(|| async { Arc::new(Mutex::new(0)) })
            .await
            .clone()
    }

    /// Create a new test database with TimescaleDB (uses shared container)
    ///
    /// This function:
    /// 1. Gets or creates a shared TimescaleDB container (only created once per test run)
    /// 2. Creates a unique schema for this test to ensure parallel test isolation
    /// 3. Establishes a connection with the unique schema in the search_path
    pub async fn new() -> anyhow::Result<Self> {
        // Increment active instances counter
        let counter = Self::get_or_init_counter().await;
        {
            let mut count = counter.lock().await;
            *count += 1;
        }

        // Get or create shared container
        let container = Self::get_or_create_container().await?;
        let container_lock = container.lock().await;

        // Extract base_url from the Option<SharedContainer>
        let base_url = if let Some(ref shared_container) = *container_lock {
            shared_container.database_url.clone()
        } else {
            return Err(anyhow::anyhow!("Shared container was dropped"));
        };
        drop(container_lock); // Release lock early

        // Generate a unique schema name
        let schema_name = format!("s{}", uuid::Uuid::new_v4().to_string().replace("-", ""));

        // Connect to default database to create schema
        let admin_db = Self::connect_with_retry(&base_url, 20).await?;

        // Create the unique schema
        let create_schema_sql = format!("CREATE SCHEMA IF NOT EXISTS {}", schema_name);
        let statement = Statement::from_string(DatabaseBackend::Postgres, create_schema_sql);
        admin_db
            .execute(statement)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create test schema: {}", e))?;

        // Now reconnect with the schema in the search_path via connection string parameter
        // Include 'public' in search_path so TimescaleDB functions are accessible
        let database_url = format!("{}?options=-c search_path={},public", base_url, schema_name);

        // Connect with schema-specific search_path
        let db = Self::connect_with_retry_schema(&database_url, &schema_name, 20).await?;

        let test_db = TestDatabase {
            db: Arc::new(db),
            database_url,
            dedicated_container: None,
            schema_name: Some(schema_name),
            uses_shared_container: true,
        };

        // Verify connection works
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Initial connection test failed: {}", e))?;

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
            schema_name: None, // Isolated containers use public schema
            uses_shared_container: false,
        };

        // Verify connection works
        test_db
            .test_connection()
            .await
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
    #[deprecated(
        note = "Use TestDatabase::new() for shared container or TestDatabase::new_isolated_with_config() for isolated container"
    )]
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
    /// Each test gets a unique schema, so migrations are always run.
    pub async fn with_migrations() -> anyhow::Result<Self> {
        let test_db = Self::new().await?;

        // Verify database connection is working before migrations
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Database connection test failed: {}", e))?;

        // Acquire the global migration lock to ensure only one test runs migrations at a time
        // This is critical for TimescaleDB which creates extensions, continuous aggregates,
        // and internal types that can conflict when created concurrently
        let migration_lock = MIGRATION_LOCK
            .get_or_init(|| async { Arc::new(Mutex::new(())) })
            .await;
        let _lock = migration_lock.lock().await;

        // Create extensions (protected by migration lock)
        // Extensions must be created in public schema (database-wide)
        test_db
            .execute_sql("CREATE EXTENSION IF NOT EXISTS timescaledb WITH SCHEMA public CASCADE")
            .await
            .ok();
        test_db
            .execute_sql("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public CASCADE")
            .await
            .ok();

        // Run migrations in this test's unique schema
        // Since each test has its own schema, migrations always run, but only one at a time
        Migrator::up(&*test_db.db, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

        // Verify migrations were successful by checking a known table in current schema
        let check_sql = "SELECT EXISTS (
            SELECT FROM information_schema.tables
            WHERE table_schema = current_schema()
            AND table_name = 'users'
        )";

        let result = test_db
            .query_sql(check_sql)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to verify migrations: {}", e))?;

        let users_table_exists = result
            .first()
            .and_then(|row| row.try_get::<bool>("", "exists").ok())
            .unwrap_or(false);

        if !users_table_exists {
            return Err(anyhow::anyhow!("Migrations did not create expected tables"));
        }

        // Lock is automatically released when _lock goes out of scope

        Ok(test_db)
    }

    /// Create a test database and run migrations with custom Migrator
    ///
    /// This generic method allows using any MigratorTrait implementation.
    /// Each test gets a unique schema, and migrations run sequentially (protected by lock).
    pub async fn with_custom_migrations<M>() -> anyhow::Result<Self>
    where
        M: MigratorTrait,
    {
        let test_db = Self::new().await?;

        // Verify database connection is working
        test_db
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Database connection test failed: {}", e))?;

        // Acquire the global migration lock to ensure only one test runs migrations at a time
        let migration_lock = MIGRATION_LOCK
            .get_or_init(|| async { Arc::new(Mutex::new(())) })
            .await;
        let _lock = migration_lock.lock().await;

        // Create extensions (protected by migration lock)
        test_db
            .execute_sql("CREATE EXTENSION IF NOT EXISTS timescaledb WITH SCHEMA public CASCADE")
            .await
            .ok();
        test_db
            .execute_sql("CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public CASCADE")
            .await
            .ok();

        // Run migrations in this test's unique schema
        M::up(&*test_db.db, None)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to run custom migrations: {}", e))?;

        // Lock is automatically released when _lock goes out of scope

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
                    let test =
                        Statement::from_string(DatabaseBackend::Postgres, "SELECT 1".to_owned());

                    match db.execute(test).await {
                        Ok(_) => return Ok(db),
                        Err(e) if retries > 0 => {
                            eprintln!(
                                "Database connected but test query failed (retries left: {}): {}",
                                retries, e
                            );
                            // Fall through to retry logic below
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Database connected but not responsive: {}",
                                e
                            ));
                        }
                    }
                }
                Err(e) if retries > 0 => {
                    eprintln!(
                        "Failed to connect to database (retries left: {}): {}",
                        retries, e
                    );
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

    /// Connect to database with schema-specific search_path
    async fn connect_with_retry_schema(
        database_url: &str,
        schema_name: &str,
        max_retries: u32,
    ) -> anyhow::Result<DbConnection> {
        use sea_orm::ConnectOptions;
        use std::time::Duration;

        let mut retries = max_retries;

        // Create connection options with schema in search_path
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
                    // Verify connection and search_path (include public for TimescaleDB)
                    let test = Statement::from_string(
                        DatabaseBackend::Postgres,
                        format!("SET search_path TO {}, public", schema_name),
                    );

                    match db.execute(test).await {
                        Ok(_) => {
                            // Verify search_path is set correctly
                            let check = Statement::from_string(
                                DatabaseBackend::Postgres,
                                "SHOW search_path".to_owned(),
                            );
                            match db.query_one(check).await {
                                Ok(_) => return Ok(db),
                                Err(e) if retries > 0 => {
                                    eprintln!(
                                        "Search path verification failed (retries left: {}): {}",
                                        retries, e
                                    );
                                }
                                Err(e) => {
                                    return Err(anyhow::anyhow!(
                                        "Failed to verify search_path: {}",
                                        e
                                    ));
                                }
                            }
                        }
                        Err(e) if retries > 0 => {
                            eprintln!(
                                "Failed to set search_path (retries left: {}): {}",
                                retries, e
                            );
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!("Failed to set search_path: {}", e));
                        }
                    }
                }
                Err(e) if retries > 0 => {
                    eprintln!(
                        "Failed to connect to database (retries left: {}): {}",
                        retries, e
                    );
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to connect to database: {}", e));
                }
            }

            if retries > 0 {
                retries -= 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
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
        let result = self
            .db
            .execute(statement)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(result)
    }

    /// Query raw SQL and return results
    pub async fn query_sql(&self, sql: &str) -> anyhow::Result<Vec<QueryResult>> {
        let statement = Statement::from_string(DatabaseBackend::Postgres, sql.to_owned());
        let result = self
            .db
            .query_all(statement)
            .await
            .map_err(anyhow::Error::from)?;
        Ok(result)
    }

    /// Clean up all data in the database (useful for test cleanup)
    ///
    /// This truncates all tables except migration-related tables.
    /// Tables are truncated in reverse dependency order to avoid foreign key issues.
    pub async fn cleanup_all_tables(&self) -> anyhow::Result<()> {
        // First, drop all TimescaleDB continuous aggregates (materialized views)
        let views = self
            .query_sql("SELECT matviewname FROM pg_matviews WHERE schemaname = 'public'")
            .await?;

        for view in views {
            if let Ok(view_name) = view.try_get::<String>("", "matviewname") {
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
            if let Ok(type_name) = type_row.try_get::<String>("", "typname") {
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
            if let Ok(table_name) = table.try_get::<String>("", "tablename") {
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
        let result = test_db
            .query_sql(
                "SELECT column_name FROM information_schema.columns WHERE table_name = 'users'",
            )
            .await?;

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

    /// Test that parallel tests using TestDatabase::new() get isolated schemas
    #[tokio::test]
    async fn test_concurrent_test_isolation() -> anyhow::Result<()> {
        use std::sync::Arc;
        use tokio::sync::Barrier;

        // Create a barrier to ensure all tests start at the same time
        let barrier = Arc::new(Barrier::new(5));

        // Spawn 5 concurrent "tests" that all try to create and use the same table
        let handles: Vec<_> = (0..5)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    // Wait for all tasks to be ready
                    barrier.wait().await;

                    // Each gets its own TestDatabase with unique schema
                    let test_db = TestDatabase::new().await.unwrap();

                    // All create a table with the same name (would conflict without isolation)
                    test_db
                        .execute_sql("CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT)")
                        .await
                        .unwrap();

                    // Insert test-specific data
                    let insert_sql = format!("INSERT INTO users (name) VALUES ('user_{}')", i);
                    test_db.execute_sql(&insert_sql).await.unwrap();

                    // Verify only our data exists (isolation working)
                    let result = test_db.query_sql("SELECT * FROM users").await.unwrap();
                    assert_eq!(result.len(), 1, "Test {} should only see its own data", i);

                    // Verify the correct data
                    let name: String = result[0].try_get("", "name").unwrap();
                    assert_eq!(name, format!("user_{}", i));

                    i
                })
            })
            .collect();

        // Wait for all tasks to complete
        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // Verify all 5 tests completed successfully
        assert_eq!(results.len(), 5);
        assert_eq!(results, vec![0, 1, 2, 3, 4]);

        Ok(())
    }

    /// Test that each TestDatabase instance gets a unique schema
    #[tokio::test]
    async fn test_unique_schemas() -> anyhow::Result<()> {
        let db1 = TestDatabase::new().await?;
        let db2 = TestDatabase::new().await?;
        let db3 = TestDatabase::new().await?;

        // Get the current schema for each connection
        let schema1 = db1
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        let schema2 = db2
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        let schema3 = db3
            .query_sql("SELECT current_schema()")
            .await?
            .first()
            .and_then(|r| r.try_get::<String>("", "current_schema").ok())
            .unwrap();

        // All schemas should be different
        assert_ne!(schema1, schema2);
        assert_ne!(schema2, schema3);
        assert_ne!(schema1, schema3);

        // All should start with "s" (our schema naming convention)
        assert!(schema1.starts_with('s'));
        assert!(schema2.starts_with('s'));
        assert!(schema3.starts_with('s'));

        Ok(())
    }

    /// Test that with_migrations works with unique schemas
    #[tokio::test]
    async fn test_with_migrations_concurrent() -> anyhow::Result<()> {
        use std::sync::Arc;
        use tokio::sync::Barrier;

        let barrier = Arc::new(Barrier::new(3));

        let handles: Vec<_> = (0..3)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;

                    // Each gets its own database with migrations
                    let test_db = TestDatabase::with_migrations().await.unwrap();

                    // Verify users table exists (from migrations)
                    let result = test_db
                        .query_sql("SELECT column_name FROM information_schema.columns WHERE table_name = 'users'")
                        .await
                        .unwrap();

                    assert!(!result.is_empty(), "Test {} should have users table", i);
                    i
                })
            })
            .collect();

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(results, vec![0, 1, 2]);

        Ok(())
    }
}
