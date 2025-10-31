# Error Tracking Guide

This guide shows you how to use the Temps Node.js SDK's Sentry-compatible error tracking in your Node.js applications.

## Installation

```bash
npm install @temps-sdk/node-sdk
```

## Quick Start

### Basic Setup

```javascript
import { ErrorTracking } from '@temps-sdk/node-sdk';

// Initialize error tracking
ErrorTracking.init({
  dsn: 'https://your-public-key@your-server.com/project-id',
  environment: process.env.NODE_ENV || 'development',
  release: '1.0.0',
  sampleRate: 1.0, // Capture 100% of errors
  tracesSampleRate: 0.1, // Capture 10% of transactions for performance monitoring
  debug: process.env.NODE_ENV === 'development'
});
```

### CommonJS Usage

```javascript
const { ErrorTracking } = require('@temps-sdk/node-sdk');

ErrorTracking.init({
  dsn: 'https://your-public-key@your-server.com/project-id',
  environment: 'production'
});
```

## Core Features

### 1. Error Capture

#### Capturing Exceptions

```javascript
try {
  riskyOperation();
} catch (error) {
  ErrorTracking.captureException(error, {
    tags: { module: 'auth', action: 'login' },
    level: 'error',
    extra: { userId: user.id, timestamp: Date.now() }
  });
}
```

#### Capturing Messages

```javascript
ErrorTracking.captureMessage('User login successful', 'info', {
  tags: { event: 'user_action' },
  extra: { userId: user.id }
});
```

#### Capturing Custom Events

```javascript
ErrorTracking.captureEvent({
  message: 'Custom application event',
  level: 'warning',
  tags: { component: 'payment-processor' },
  extra: { transactionId: 'txn_123' }
});
```

### 2. User Context

```javascript
// Set user information globally
ErrorTracking.setUser({
  id: '12345',
  username: 'john_doe',
  email: 'john@example.com',
  ip_address: '192.168.1.1'
});

// Clear user context
ErrorTracking.setUser(null);
```

### 3. Tags and Context

```javascript
// Set global tags
ErrorTracking.setTags({
  server: 'web-01',
  version: '2.1.0',
  environment: 'production'
});

ErrorTracking.setTag('feature_flag', 'new_ui_enabled');

// Set extra context data
ErrorTracking.setExtras({
  build_number: '1234',
  commit_hash: 'abc123'
});

ErrorTracking.setExtra('database_host', 'db.example.com');

// Set structured context
ErrorTracking.setContext('database', {
  name: 'primary',
  version: '14.2',
  host: 'db.example.com'
});
```

### 4. Breadcrumbs

```javascript
// Add navigation breadcrumb
ErrorTracking.addBreadcrumb({
  message: 'User navigated to checkout',
  category: 'navigation',
  level: 'info',
  data: { from: '/cart', to: '/checkout' }
});

// Add HTTP request breadcrumb
ErrorTracking.addBreadcrumb({
  message: 'API request',
  category: 'http',
  level: 'info',
  data: {
    method: 'POST',
    url: '/api/payments',
    status_code: 200
  }
});

// Clear all breadcrumbs
ErrorTracking.clearBreadcrumbs();
```

### 5. Scoped Context

#### Using `withScope`

```javascript
ErrorTracking.withScope((scope) => {
  scope.setTag('request_id', 'req_123');
  scope.setLevel('warning');
  scope.setContext('request', {
    method: 'POST',
    url: '/api/users',
    headers: { 'content-type': 'application/json' }
  });

  // This error will include the scoped context
  ErrorTracking.captureException(new Error('Validation failed'));
});

// The scoped context is not applied here
ErrorTracking.captureMessage('Outside of scope');
```

#### Configuring Global Scope

```javascript
ErrorTracking.configureScope((scope) => {
  scope.setTag('server', 'web-01');
  scope.setContext('app', {
    name: 'my-app',
    version: '1.0.0'
  });
});
```

## Performance Monitoring

### 1. Transactions

Transactions help you monitor the performance of operations in your application.

```javascript
// Start a transaction
const transaction = ErrorTracking.startTransaction({
  name: 'User Registration',
  op: 'auth.register'
});

try {
  // Your operation
  await registerUser(userData);

  transaction.setStatus('ok');
} catch (error) {
  transaction.setStatus('internal_error');
  ErrorTracking.captureException(error);
} finally {
  transaction.finish();
}
```

### 2. Spans

Spans help you monitor individual operations within a transaction.

```javascript
const transaction = ErrorTracking.startTransaction({
  name: 'Database Migration',
  op: 'db.migration'
});

// Create spans for individual operations
const validateSpan = transaction.startChild({
  op: 'db.validate',
  description: 'Validate migration scripts'
});

await validateMigrations();
validateSpan.finish();

const migrateSpan = transaction.startChild({
  op: 'db.migrate',
  description: 'Run migration scripts'
});

await runMigrations();
migrateSpan.setTag('tables_affected', '5');
migrateSpan.finish();

transaction.finish();
```

### 3. Automatic Transaction Tracking

```javascript
// Express.js middleware example
app.use((req, res, next) => {
  const transaction = ErrorTracking.startTransaction({
    name: `${req.method} ${req.route?.path || req.path}`,
    op: 'http.server'
  });

  res.on('finish', () => {
    transaction.setTag('http.status_code', res.statusCode.toString());
    transaction.setData('http.response.size', res.get('content-length'));

    if (res.statusCode >= 400) {
      transaction.setStatus('invalid_argument');
    } else {
      transaction.setStatus('ok');
    }

    transaction.finish();
  });

  next();
});
```

## Advanced Features

### 1. User Feedback

```javascript
// Capture user feedback for a specific error
const eventId = ErrorTracking.captureException(error);

ErrorTracking.captureUserFeedback({
  event_id: eventId,
  name: 'John Doe',
  email: 'john@example.com',
  comments: 'I was trying to upload a file when this happened.'
});
```

### 2. Custom Integrations

```javascript
class DatabaseIntegration {
  constructor() {
    this.name = 'DatabaseIntegration';
  }

  setupOnce() {
    // Set up database error monitoring
    db.on('error', (error) => {
      ErrorTracking.addBreadcrumb({
        message: 'Database error occurred',
        category: 'database',
        level: 'error',
        data: { error: error.message }
      });
    });
  }
}

ErrorTracking.init({
  dsn: 'your-dsn',
  integrations: [new DatabaseIntegration()]
});
```

### 3. Filtering Events

```javascript
ErrorTracking.init({
  dsn: 'your-dsn',
  beforeSend: (event) => {
    // Filter out events in development
    if (event.environment === 'development') {
      return null; // Don't send the event
    }

    // Remove sensitive data
    if (event.extra?.password) {
      delete event.extra.password;
    }

    return event;
  },
  ignoreErrors: [
    'NetworkError',
    /^Non-Error promise rejection captured/
  ]
});
```

### 4. Sampling

```javascript
ErrorTracking.init({
  dsn: 'your-dsn',
  sampleRate: 0.25, // Only send 25% of error events
  tracesSampleRate: 0.1 // Only send 10% of performance data
});
```

## API Reference

### Utility Functions

```javascript
// Get the last captured event ID
const eventId = ErrorTracking.getLastEventId();

// Check if error tracking is enabled
if (ErrorTracking.isEnabled()) {
  // Do something
}

// Get current transaction
const transaction = ErrorTracking.getCurrentTransaction();

// Set global error level
ErrorTracking.setLevel('warning');

// Get the client instance
const client = ErrorTracking.getCurrentClient();
```

### Cleanup

```javascript
// Flush pending events before shutdown
await ErrorTracking.flush(2000); // Wait up to 2 seconds

// Close the client
await ErrorTracking.close(2000);
```

## Express.js Integration Example

```javascript
import express from 'express';
import { ErrorTracking } from '@temps-sdk/node-sdk';

const app = express();

// Initialize error tracking
ErrorTracking.init({
  dsn: 'your-dsn',
  environment: process.env.NODE_ENV,
  tracesSampleRate: 0.1
});

// Request tracing middleware
app.use((req, res, next) => {
  const transaction = ErrorTracking.startTransaction({
    name: `${req.method} ${req.path}`,
    op: 'http.server'
  });

  req.transaction = transaction;

  res.on('finish', () => {
    transaction.setTag('http.status_code', res.statusCode.toString());
    transaction.setTag('http.method', req.method);
    transaction.setData('http.url', req.url);

    if (res.statusCode >= 400) {
      transaction.setStatus('invalid_argument');
    } else {
      transaction.setStatus('ok');
    }

    transaction.finish();
  });

  next();
});

// Routes
app.get('/api/users/:id', async (req, res) => {
  const span = req.transaction.startChild({
    op: 'db.query',
    description: 'Get user by ID'
  });

  try {
    const user = await getUserById(req.params.id);
    span.setStatus('ok');
    res.json(user);
  } catch (error) {
    span.setStatus('internal_error');
    ErrorTracking.captureException(error, {
      tags: { userId: req.params.id }
    });
    res.status(500).json({ error: 'Internal server error' });
  } finally {
    span.finish();
  }
});

// Error handling middleware
app.use((error, req, res, next) => {
  ErrorTracking.captureException(error, {
    tags: {
      path: req.path,
      method: req.method
    },
    extra: {
      body: req.body,
      query: req.query,
      params: req.params
    }
  });

  res.status(500).json({ error: 'Internal server error' });
});

export default app;
```

## Next.js Integration Example

```javascript
// pages/_app.js
import { ErrorTracking } from '@temps-sdk/node-sdk';
import { useEffect } from 'react';

ErrorTracking.init({
  dsn: process.env.NEXT_PUBLIC_SENTRY_DSN,
  environment: process.env.NODE_ENV,
  tracesSampleRate: 0.1
});

function MyApp({ Component, pageProps }) {
  useEffect(() => {
    // Set user context if available
    if (pageProps.user) {
      ErrorTracking.setUser({
        id: pageProps.user.id,
        email: pageProps.user.email
      });
    }
  }, [pageProps.user]);

  return <Component {...pageProps} />;
}

export default MyApp;

// pages/api/users.js
export default async function handler(req, res) {
  const transaction = ErrorTracking.startTransaction({
    name: 'GET /api/users',
    op: 'http.server'
  });

  try {
    const users = await getUsers();
    transaction.setStatus('ok');
    res.status(200).json(users);
  } catch (error) {
    transaction.setStatus('internal_error');
    ErrorTracking.captureException(error);
    res.status(500).json({ error: 'Failed to fetch users' });
  } finally {
    transaction.finish();
  }
}
```

## Testing

```javascript
// In your test files
import { ErrorTracking } from '@temps-sdk/node-sdk';

beforeEach(() => {
  // Reset error tracking for tests
  ErrorTracking.__resetForTesting();
});

// Mock error tracking in tests
jest.mock('@temps-sdk/node-sdk', () => ({
  ErrorTracking: {
    init: jest.fn(),
    captureException: jest.fn(),
    captureMessage: jest.fn(),
    setUser: jest.fn(),
    // ... other methods
  }
}));
```

## Best Practices

1. **Initialize Early**: Call `ErrorTracking.init()` as early as possible in your application
2. **Use Breadcrumbs**: Add breadcrumbs to track user actions leading up to errors
3. **Set Context**: Use user context, tags, and extra data to make errors more actionable
4. **Filter Sensitive Data**: Use `beforeSend` to remove passwords, tokens, and other sensitive information
5. **Performance Monitoring**: Use transactions and spans to monitor critical operations
6. **Graceful Degradation**: Always handle cases where error tracking might fail
7. **Environment-Specific Config**: Use different sample rates and debug settings per environment

## Troubleshooting

### Common Issues

1. **Events not being sent**: Check your DSN and network connectivity
2. **Too much data**: Increase sample rates or filter events in `beforeSend`
3. **Missing context**: Ensure you're setting user and tag information early
4. **Performance impact**: Adjust `tracesSampleRate` to reduce overhead

### Debug Mode

```javascript
ErrorTracking.init({
  dsn: 'your-dsn',
  debug: true // This will log events to console instead of sending them
});
```

For more advanced configuration options, see the [API documentation](./API.md).
