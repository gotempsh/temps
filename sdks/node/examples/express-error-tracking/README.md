# Express Error Tracking Example

This example demonstrates how to use the Temps SDK for error tracking in an Express.js application.

## Setup

1. Install dependencies:
```bash
cd examples/express-error-tracking
bun install
```

2. Run the server:
```bash
bun run start
# or for development with auto-reload
bun run dev
```

## Testing Error Tracking

Once the server is running on `http://localhost:3000`, you can test error tracking with these endpoints:

### 1. Basic 500 Error
```bash
curl http://localhost:3000/test-error
```
This throws a synchronous error that will be caught and tracked.

### 2. Async Error
```bash
curl http://localhost:3000/test-async-error
```
Tests async error handling in Express routes.

### 3. Custom Error with Metadata
```bash
curl http://localhost:3000/test-custom-error
```
Demonstrates tracking errors with custom metadata and status codes.

### 4. Unhandled Promise Rejection
```bash
curl http://localhost:3000/test-unhandled
```
Triggers an unhandled promise rejection after 1 second to test process-level error tracking.

### 5. Health Check
```bash
curl http://localhost:3000/health
```
Simple health check endpoint that doesn't throw errors.

## Monitoring

The server logs will show:
- When errors are thrown
- Error details being sent to Temps
- Debug information about the SDK operations

Check your Temps dashboard at the configured endpoint to see the tracked errors.

## Configuration

The example is configured with:
- **Base URL**: `http://localhost`
- **App ID**: `1`
- **Debug mode**: Enabled to show detailed logging
- **Error tracking**: Enabled for automatic error capture
- **Request logging**: Enabled to log all HTTP requests

## Features Demonstrated

- Express middleware integration
- Synchronous error handling
- Asynchronous error handling
- Custom error metadata
- Unhandled rejection tracking
- Uncaught exception tracking
- Request context in error reports
