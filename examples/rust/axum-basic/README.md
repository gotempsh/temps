# Rust Axum Basic Example

Simple Rust Axum API server for testing Rust deployments.

## Running Locally

```bash
# Build and run
cargo run

# Or build for release
cargo build --release
./target/release/axum-basic
```

## Endpoints

- `GET /` - Hello message with JSON response
- `GET /health` - Health check endpoint

## Environment Variables

- `PORT` - Server port (default: 3000)
