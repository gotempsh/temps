# Temps Deployments - Testing Guide

## Test Structure

Unit tests live in a `#[cfg(test)] mod tests` block at the bottom of the file
they cover, as everywhere else in the workspace. Integration tests that drive
the crate through its public API live in `tests/`:

```
tests/
├── common/                              # Shared test utilities
├── fixtures/                            # Sample repositories and inputs
├── configure_crons_test.rs
├── container_cleanup_on_failure_test.rs
├── multiarch_build_test.rs
├── multinode_integration_test.rs
├── nodejs_integration_test.rs
├── public_repo_deployment_test.rs
├── traefik_discovery_admin_test.rs
└── workflow_execution_service_test.rs
```

## Running Tests

```bash
# Unit tests
cargo test --lib -p temps-deployments

# A single unit test
cargo test --lib -p temps-deployments test_name

# Integration tests in tests/
cargo test -p temps-deployments --test workflow_execution_service_test

# With output and logs
RUST_LOG=debug cargo test --lib -p temps-deployments test_name -- --nocapture
```

No feature flag is needed. Tests that need Docker or a database check for it
at runtime and skip with a message when it is unavailable, so they never need
`#[ignore]`.

## Prerequisites

- Rust toolchain
- Docker, for `TestDatabase` (TimescaleDB) and the container tests. Without it
  those tests skip.

## Environment Variables

```bash
# Optional: Configure test database
export TEST_DATABASE_URL="postgresql://test_user:test_password@localhost:5432/test_db"

# Optional: Docker configuration for integration tests
export DOCKER_HOST="unix:///var/run/docker.sock"
```

## Test Data Cleanup

`TestDatabase` drops its schema on drop, and the container tests remove the
containers they create, including on failure paths.
