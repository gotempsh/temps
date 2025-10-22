# temps-database

Database connection utilities and test helpers for the Temps.

## Features

### Basic Usage

```rust
use temps_database::{establish_connection, DbConnection};

let db = establish_connection("postgresql://user:pass@localhost/db").await?;
```

### Test Utilities

The test utilities are available when using the `test-utils` feature or in test mode.

#### Using in Tests

Add to your `Cargo.toml`:

```toml
[dev-dependencies]
temps-database = { path = "../temps-database", features = ["test-utils"] }
```

#### Basic Test Database Setup

```rust
use temps_database::test_utils::TestDatabase;

#[tokio::test]
async fn test_with_database() {
    // Create a test database with TimescaleDB
    let test_db = TestDatabase::new().await.unwrap();

    // Use the database connection
    let db = test_db.connection();

    // Run your tests...

    // Database is automatically cleaned up when test_db is dropped
}
```

#### With Migrations

```rust
use temps_database::test_utils::TestDatabase;
use temps_migrations::Migrator;

#[tokio::test]
async fn test_with_migrations() {
    // Create test database and run migrations
    let test_db = TestDatabase::with_migrations::<Migrator>().await.unwrap();

    // Your database now has all migrations applied
    let db = test_db.connection();

    // Run your tests...
}
```

#### Custom Configuration

```rust
use temps_database::test_utils::TestDatabase;

#[tokio::test]
async fn test_with_custom_config() {
    let test_db = TestDatabase::with_config(
        "custom_db_name",
        "custom_user",
        "custom_password"
    ).await.unwrap();

    // Use the database...
}
```

#### Test Helpers

```rust
use temps_database::test_utils::TestDatabase;

#[tokio::test]
async fn test_with_helpers() {
    let test_db = TestDatabase::new().await.unwrap();

    // Execute raw SQL
    test_db.execute_sql("CREATE TABLE test (id INT)").await.unwrap();

    // Query raw SQL
    let results = test_db.query_sql("SELECT * FROM test").await.unwrap();

    // Clean up all tables (except migration tables)
    test_db.cleanup_all_tables().await.unwrap();

    // Create TimescaleDB hypertable
    test_db.create_hypertable("events", "timestamp").await.unwrap();
}
```

#### Transaction Testing

```rust
use temps_database::test_utils::{TestDatabase, TestTransaction};

#[tokio::test]
async fn test_with_transaction() {
    let test_db = TestDatabase::new().await.unwrap();

    // Start a transaction
    let tx = TestTransaction::new(test_db.connection_arc()).await.unwrap();

    // Do work in transaction...

    // Rollback (or commit) the transaction
    tx.rollback().await.unwrap();
}
```

## Helper Functions

- `generate_test_db_name(prefix)` - Generate unique database names for tests
- `wait_for(condition, timeout_secs, check_interval_ms)` - Wait for async conditions

## Best Practices

1. Use `TestDatabase::new()` for simple tests without specific requirements
2. Use `TestDatabase::with_migrations()` when you need the full schema
3. The database container is automatically cleaned up when the `TestDatabase` struct is dropped
4. Each test gets its own isolated database instance
5. Tests run in parallel safely as each has its own container
