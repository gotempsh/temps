# Temps Deployments - Testing Guide

This crate contains comprehensive tests for the deployment service, organized into unit tests and integration tests.

## Test Structure

```
tests/
├── unit/                           # Unit tests with mocked dependencies
│   ├── deployment_service_unit_tests.rs
│   └── mod.rs
├── integration/                    # Integration tests with real services
│   ├── deployment_service_integration_tests.rs
│   ├── common/                     # Shared test utilities
│   │   └── mod.rs
│   └── mod.rs
```

## Running Tests

### Unit Tests (Fast, No External Dependencies)
```bash
# Run only unit tests (default)
cargo test --lib

# Run specific unit test
cargo test test_unit_deployment_to_container_launch_spec

# Run all unit tests with pattern
cargo test unit_tests
```

### Integration Tests (Requires Docker)
```bash
# Run integration tests (requires Docker to be running)
cargo test --features integration-tests integration_tests

# Run specific integration test
cargo test --features integration-tests test_integration_deployment_to_container_launch_spec

# Run ignored integration tests (Docker-dependent)
cargo test --features integration-tests -- --ignored
```

### All Tests
```bash
# Run both unit and integration tests
cargo test --features integration-tests
```

## Test Types

### Unit Tests
- **Purpose**: Fast, isolated testing with mocked dependencies
- **Dependencies**: Mock services using `mockall`
- **Database**: TestDatabase with TimescaleDB (isolated)
- **Speed**: Very fast (~seconds)
- **Coverage**: Business logic, error handling, data transformations

**Unit test examples:**
- `test_unit_deployment_to_container_launch_spec()` - Environment variable mapping
- `test_unit_pause_deployment()` - Deployment pause logic
- `test_unit_resume_deployment()` - Deployment resume logic
- `test_unit_rollback_to_deployment()` - Rollback logic with mock containers

### Integration Tests
- **Purpose**: End-to-end testing with real services
- **Dependencies**: Real Docker, real database, real queue service
- **Database**: TestDatabase with real PostgreSQL/TimescaleDB
- **Speed**: Slower (~minutes)
- **Coverage**: Full service integration, container operations, database transactions

**Integration test examples:**
- `test_integration_container_lifecycle()` - Real container pause/resume/teardown
- `test_integration_rollback_with_real_containers()` - Real container rollback
- `test_integration_environment_teardown()` - Multi-deployment environment cleanup
- `test_integration_database_operations()` - Real database queries and mappings

## Prerequisites

### For Unit Tests
- Rust toolchain
- Docker (for TestDatabase TimescaleDB container)

### For Integration Tests
- All unit test requirements, plus:
- Docker daemon running locally
- Network access for Docker operations
- Sufficient disk space for container images

## Environment Variables

```bash
# Optional: Configure test database
export TEST_DATABASE_URL="postgresql://test_user:test_password@localhost:5432/test_db"

# Optional: Docker configuration for integration tests
export DOCKER_HOST="unix:///var/run/docker.sock"
```

## Test Configuration

The tests are configured in `Cargo.toml`:

```toml
[features]
integration-tests = []

[[test]]
name = "unit_tests"
path = "tests/unit/deployment_service_unit_tests.rs"

[[test]]
name = "integration_tests"
path = "tests/integration/deployment_service_integration_tests.rs"
required-features = ["integration-tests"]
```

## Continuous Integration

For CI environments:

```bash
# Fast feedback loop (unit tests only)
cargo test --lib

# Full validation (requires Docker service)
cargo test --features integration-tests
```

## Debugging Tests

```bash
# Run with output
cargo test --features integration-tests -- --nocapture

# Run specific test with debug info
RUST_LOG=debug cargo test --features integration-tests test_integration_container_lifecycle -- --nocapture

# Run with backtraces
RUST_BACKTRACE=1 cargo test --features integration-tests
```

## Test Data Cleanup

Both test types automatically clean up their test data:
- Unit tests: Automatic cleanup via TestDatabase drop
- Integration tests: Explicit cleanup in each test + Docker container removal

## Architecture Benefits

This dual testing approach provides:

1. **Fast Feedback**: Unit tests run quickly during development
2. **Confidence**: Integration tests validate real-world scenarios
3. **Isolation**: Unit tests don't depend on external services
4. **Comprehensive Coverage**: Both business logic and integration points
5. **Scalability**: Can run unit tests in environments without Docker