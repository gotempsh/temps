# Integration Tests

This directory contains integration tests for the temps-deployments crate.

## Test Files

### `nodejs_integration_test.rs`

Tests the complete three-stage deployment pipeline using a simple Node.js application:

**Stages:**
1. **Download Repo** - Copies fixture from `fixtures/simple-nodejs/`
2. **Build Image** - Builds Docker image using the Dockerfile
3. **Deploy Image** - Simulates deployment to Kubernetes

**Tests:**
- `test_nodejs_fixture_exists` - Validates fixture files exist
- `test_nodejs_package_json_valid` - Validates package.json is valid JSON
- `test_nodejs_three_stage_deployment` - Full three-stage integration test (requires Docker, marked as `#[ignore]`)

### Running Tests

```bash
# Run all non-ignored tests (no Docker required)
cargo test --test nodejs_integration_test

# Run full integration test including Docker build
cargo test --test nodejs_integration_test --ignored

# Run specific test
cargo test --test nodejs_integration_test test_nodejs_fixture_exists
```

## Fixtures

Test fixtures are located in `fixtures/` directory:

- **`simple-nodejs/`** - Minimal Node.js + Express application
  - `Dockerfile` - Docker build configuration
  - `package.json` - Node.js dependencies
  - `index.js` - Simple HTTP server
  - `README.md` - Documentation

## Architecture

The tests use a mock `GitProviderManagerTrait` implementation that copies files from the local fixture directory instead of cloning from a real git repository. This allows testing the complete workflow without external dependencies.

### Key Components

- **`LocalFixtureGitProvider`** - Mock implementation that copies fixture files
- **`copy_dir_recursive()`** - Helper to recursively copy directory contents
- **Job Builders** - DownloadRepoBuilder, BuildImageJobBuilder, DeployImageJobBuilder
- **WorkflowBuilder** - Orchestrates the three jobs with proper dependencies

## Adding New Fixtures

To add a new test fixture:

1. Create directory under `fixtures/` (e.g., `fixtures/python-app/`)
2. Add application files (source code, Dockerfile, etc.)
3. Create integration test in new file (e.g., `python_integration_test.rs`)
4. Use `LocalFixtureGitProvider` to copy fixture files
5. Build jobs using the job builders
6. Verify with WorkflowBuilder

Example structure:
```
fixtures/
  my-app/
    Dockerfile
    src/
    README.md
tests/
  my_app_integration_test.rs
```