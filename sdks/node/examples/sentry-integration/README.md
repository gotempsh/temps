# Sentry Integration Example

This example demonstrates how to use the official Sentry SDK with a custom Temps endpoint for error tracking and performance monitoring.

## Setup

1. Install dependencies:
```bash
cd examples/sentry-integration
bun install
```

2. Run the server:
```bash
bun run start
# or for development with auto-reload
bun run dev
```

The server runs on port 3001 to avoid conflicts with the other example.

## Configuration

The Sentry SDK is configured to send data to your custom endpoint:
- **DSN**: `http://oS4BvLtdW0IaoLmWrgLpLBAleXQLPfyd9S4siuF_-kk@localhost/1`
- **Custom Headers**: Basic auth with your API credentials
- **Debug Mode**: Enabled to see what's being sent
- **Traces Sample Rate**: 100% (for testing)
- **Profiles Sample Rate**: 100% (for testing)

## Testing Error Tracking

### 1. Basic Error
```bash
curl http://localhost:3001/test-error
```
Throws a basic unhandled error that Sentry will capture.

### 2. Warning Message
```bash
curl http://localhost:3001/test-warning
```
Sends a warning-level message to Sentry without throwing an error.

### 3. Performance Transaction
```bash
curl http://localhost:3001/test-transaction
```
Creates a performance transaction with child spans to test performance monitoring.

### 4. Error with Breadcrumbs
```bash
curl http://localhost:3001/test-breadcrumb
```
Adds breadcrumbs before throwing an error. Breadcrumbs provide context about what happened before the error.

### 5. Error with User Context
```bash
curl http://localhost:3001/test-user-context
```
Attaches user information and additional context before throwing an error.

### 6. Error with Custom Tags
```bash
curl http://localhost:3001/test-tags
```
Demonstrates adding custom tags and extra data to errors for better filtering and searching.

### 7. Handled 500 Error
```bash
curl http://localhost:3001/test-500
```
Returns a 500 status while properly logging the error to Sentry.

### 8. Debug Test
```bash
curl http://localhost:3001/debug-sentry
```
Sends a debug message to verify Sentry is configured correctly.

### 9. Health Check
```bash
curl http://localhost:3001/health
```
Shows the current Sentry configuration and connection status.

## Features Demonstrated

### Error Tracking
- Unhandled exceptions
- Handled errors with `captureException()`
- Custom error properties
- Error levels (error, warning, info, debug)

### Context & Enrichment
- User context with `setUser()`
- Custom tags with `setTag()`
- Extra data with `setExtra()`
- Breadcrumbs for error context
- Custom contexts with `setContext()`

### Performance Monitoring
- Transaction creation
- Child span tracking
- Operation types (db.query, http.request)
- Automatic Express.js instrumentation

### Integrations
- HTTP call tracing
- Express.js middleware tracing
- Profiling integration
- Auto session tracking

## Monitoring

The server logs show:
- Event IDs being sent to Sentry
- Transaction details
- Debug information from Sentry SDK
- All events are logged via `beforeSend` and `beforeSendTransaction` hooks

## Production Considerations

For production use, you should:
1. Set `debug: false`
2. Adjust `tracesSampleRate` to a lower value (e.g., 0.1 for 10%)
3. Adjust `profilesSampleRate` to a lower value
4. Use environment variables for sensitive configuration
5. Implement proper error filtering in `beforeSend`
