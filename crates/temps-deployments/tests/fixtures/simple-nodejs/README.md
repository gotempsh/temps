# Simple Node.js Test Fixture

This is a minimal Node.js application used for integration testing the three-stage deployment pipeline.

## Structure

- **`Dockerfile`** - Multi-stage Docker build configuration
- **`package.json`** - Node.js dependencies (Express)
- **`index.js`** - Simple HTTP server with health check
- **`package-lock.json`** - Dependency lock file

## Application

The app runs an Express server on port 3000 with two endpoints:

- `GET /` - Returns JSON with message, version, and timestamp
- `GET /health` - Health check endpoint

## Usage in Tests

This fixture is used by `tests/nodejs_integration_test.rs` to test:

1. **Download Repo Stage** - Copies fixture files to temp directory
2. **Build Image Stage** - Builds Docker image from the Dockerfile
3. **Deploy Image Stage** - Simulates deployment of the built image

## Running the Test

```bash
# Run fixture validation tests (no Docker required)
cargo test --test nodejs_integration_test test_nodejs_fixture

# Run full three-stage deployment test (requires Docker)
cargo test --test nodejs_integration_test test_nodejs_three_stage_deployment --ignored
```

## Testing Locally

You can test the Docker image manually:

```bash
cd tests/fixtures/simple-nodejs

# Build the image
docker build -t nodejs-test-app:latest .

# Run the container
docker run -p 3000:3000 nodejs-test-app:latest

# Test the endpoints
curl http://localhost:3000/
curl http://localhost:3000/health
```