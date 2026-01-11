# External Service Backup/Restore Test Utilities

## Overview

Created reusable test utilities in `crates/temps-providers/src/externalsvc/test_utils.rs` to simplify backup and restore testing for all external services.

## Components

### 1. MinioTestContainer

A self-contained MinIO (S3-compatible) test container that:
- Automatically pulls and starts a MinIO Docker container
- Finds available ports
- Creates S3 client with proper configuration
- Creates and manages S3 buckets
- Provides automatic cleanup

**Usage**:
```rust
let minio = MinioTestContainer::start(docker.clone(), "test-bucket")
    .await
    .expect("Failed to start MinIO");

// Use minio.s3_client, minio.s3_source, minio.bucket_name, etc.

// Cleanup when done
minio.cleanup().await?;
```

### 2. Mock Entity Creators

Helper functions to create properly structured mock entities:

- `create_mock_backup(subpath: &str)` - Creates temps_entities::backups::Model
- `create_mock_external_service(name, service_type, version)` - Creates temps_entities::external_services::Model
- `create_mock_db()` - Creates in-memory SQLite database connection

**Usage**:
```rust
let backup_record = create_mock_backup("backups/test");
let external_service = create_mock_external_service(
    "test-service".to_string(),
    "mongodb",
    "8.0"
);
let db_conn = create_mock_db().await?;
```

## Benefits

1. **Eliminates Duplication**: No need to copy-paste MinIO setup across tests
2. **Consistency**: All tests use the same properly configured S3 infrastructure
3. **Simplicity**: Reduces test setup from ~100 lines to ~5 lines
4. **Maintainability**: Changes to test infrastructure only need to be made in one place
5. **Correctness**: Ensures mock entities match current schema

## Before & After Comparison

### Before (Manual Setup)
```rust
// ~80 lines of MinIO container setup
let mut pull_stream = docker.create_image(...);
let minio_config = bollard::models::ContainerCreateBody { ... };
let minio_container = docker.create_container(...).await?;
docker.start_container(...).await?;
tokio::time::sleep(...).await;

let s3_config = aws_sdk_s3::config::Builder::new()...;
let s3_client = aws_sdk_s3::Client::from_conf(s3_config);
s3_client.create_bucket()...;

// ~40 lines of mock entity creation
let s3_source = temps_entities::s3_sources::Model { ... };
let backup_record = temps_entities::backups::Model { ... };
let external_service = temps_entities::external_services::Model { ... };
let db_conn = sea_orm::Database::connect(...).await?;

// Manual cleanup ~20 lines
docker.stop_container(...).await;
docker.remove_container(...).await;
```

### After (Using Test Utilities)
```rust
use super::super::test_utils::{
    create_mock_backup, create_mock_db, create_mock_external_service,
    MinioTestContainer,
};

// Single line to start MinIO
let minio = MinioTestContainer::start(docker.clone(), "test-bucket").await?;

// Three lines to create mock entities
let backup_record = create_mock_backup("backups/test");
let db_conn = create_mock_db().await?;
let external_service = create_mock_external_service(name, "mongodb", "8.0");

// Use minio.s3_client, minio.s3_source, etc.

// Single line cleanup
minio.cleanup().await?;
```

## Usage in MongoDB Test

The MongoDB backup/restore test (`test_mongodb_backup_and_restore_to_s3`) demonstrates the usage pattern:

1. Start MinIO container
2. Create MongoDB container and insert test data
3. Create mock entities using utilities
4. Backup to S3
5. Drop database to simulate data loss
6. Restore from S3
7. Verify restored data
8. Cleanup both containers

This pattern can be replicated for PostgreSQL, Redis, S3, and any other external services.

## Future Services

When adding backup/restore tests for other services (PostgreSQL, Redis, S3), simply:

1. Import the test utilities
2. Start MinIO container
3. Create service-specific test data
4. Create mock entities using utilities
5. Test backup_to_s3 and restore_from_s3 methods
6. Cleanup

No need to reimplement MinIO setup or mock entity creation.

## Test Utilities Location

- **File**: `crates/temps-providers/src/externalsvc/test_utils.rs`
- **Module**: `#[cfg(test)]` only (not included in production builds)
- **Registration**: Added to `mod.rs` as `#[cfg(test)] pub mod test_utils;`

## Running Tests

```bash
# Run MongoDB backup/restore test
cargo test --lib -p temps-providers test_mongodb_backup_and_restore_to_s3 -- --nocapture

# Run all test_utils tests
cargo test --lib -p temps-providers test_utils -- --nocapture
```

## Docker Requirement

Tests gracefully skip if Docker is unavailable:
- Check Docker connection
- Verify Docker daemon is responding
- Skip test with informative message if Docker not available
