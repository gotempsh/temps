# Shared Test Database Strategy

## The Problem
Previously, each test was creating its own PostgreSQL container, which meant:
- 30 tests = 30 containers = 30x startup time
- Each container takes ~5 seconds to start
- Migrations run 30 times
- Total overhead: ~150 seconds just for container setup!

## The Solution: SharedTestDatabase

Now we have ONE container that is lazily initialized and shared across all tests:

```rust
use temps_database::shared_test_db::SharedTestDatabase;

// First test that calls this initializes the container
let db = SharedTestDatabase::with_migrations().await?;

// All subsequent tests reuse the same container (instant!)
let db2 = SharedTestDatabase::with_migrations().await?;
```

## Migration Pattern for Auth Tests

To migrate the auth tests to use the shared database:

### Before (creates new container per test):
```rust
async fn setup_test_env() -> (TestDatabase, AuthService, Arc<MockEmailService>) {
    let db = TestDatabase::with_migrations().await.unwrap();
    // ... rest of setup
}
```

### After (reuses shared container):
```rust
use temps_database::shared_test_db::SharedTestDatabase;

async fn setup_test_env() -> (SharedTestDatabase, AuthService, Arc<MockEmailService>) {
    let db = SharedTestDatabase::with_migrations().await.unwrap();

    // IMPORTANT: Clean up data from previous tests
    // Option 1: Delete specific tables
    let _ = db.execute_sql("DELETE FROM sessions").await;
    let _ = db.execute_sql("DELETE FROM users").await;

    // Option 2: Use TRUNCATE for faster cleanup
    let _ = db.execute_sql("TRUNCATE TABLE sessions, users CASCADE").await;

    // Create default settings
    let settings = settings::ActiveModel {
        id: Set(1),
        data: Set(serde_json::json!({
            "external_url": "https://test.example.com"
        })),
        ..Default::default()
    };

    // Use INSERT ... ON CONFLICT to handle existing settings
    let _ = db.execute_sql(
        "INSERT INTO settings (id, data) VALUES (1, '{\"external_url\":\"https://test.example.com\"}'::jsonb)
         ON CONFLICT (id) DO UPDATE SET data = EXCLUDED.data"
    ).await;

    let email_service = Arc::new(MockEmailService::new());
    let auth_service = AuthService::new(db.db.clone(), email_service.clone());
    (db, auth_service, email_service)
}
```

## Test Isolation Strategies

### 1. Database Cleanup (Current Approach)
Clean data between tests:
```rust
// At start of each test
db.execute_sql("TRUNCATE TABLE users, sessions CASCADE").await?;
```

### 2. Transaction Rollback (Future Enhancement)
Each test runs in a transaction that's rolled back:
```rust
let tx = db.begin_transaction().await?;
// Run test...
// Transaction automatically rolls back when dropped
```

### 3. Schema Isolation (For DDL Tests)
Tests that need to create/alter tables:
```rust
let db = SharedTestDatabase::with_isolated_schema().await?;
// This test gets its own schema
```

## Benefits

1. **Speed**: Container starts once, not 30 times
2. **Resource Usage**: One container instead of 30
3. **Predictability**: Migrations run once, ensuring consistent state
4. **Debugging**: Easier to inspect single database during test runs

## Performance Comparison

- **Before**: 30 tests × 5 seconds = 150 seconds overhead
- **After**: 1 initialization × 5 seconds = 5 seconds overhead
- **Savings**: 145 seconds (96% reduction!)

## Important Notes

1. Tests may run in parallel, so ensure proper data isolation
2. Use unique identifiers (UUIDs) for test data when possible
3. Clean up data at the START of tests, not the end (in case previous test failed)
4. The container stays alive for the entire test run and is cleaned up at process exit
