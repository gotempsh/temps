const express = require('express');
const { TempsClient } = require('@temps-sdk/node');

const app = express();
const port = process.env.PORT || 3000;

// Initialize Temps client with custom endpoint
const temps = new TempsClient({
  baseUrl: 'http://localhost',
  apiKey: 'oS4BvLtdW0IaoLmWrgLpLBAleXQLPfyd9S4siuF_-kk:DD763bN8ff_b71lGqn6lRtM5nv6VJGxJ6LAGtpPHsZ0',
  appId: '1',
  enableErrorTracking: true,
  enableRequestLogging: true,
  debug: true // Enable debug mode to see what's being sent
});

// Apply Temps middleware
app.use(temps.middleware());

// Apply error tracking middleware
app.use(temps.errorMiddleware());

// Root route
app.get('/', (req, res) => {
  res.json({
    message: 'Express Error Tracking Example',
    routes: {
      '/': 'This info page',
      '/test-error': 'Throws a 500 error',
      '/test-async-error': 'Throws an async error',
      '/test-custom-error': 'Throws a custom error with details',
      '/test-unhandled': 'Triggers an unhandled rejection'
    }
  });
});

// Route that throws a synchronous 500 error
app.get('/test-error', (req, res) => {
  console.log('About to throw a test error...');
  throw new Error('Test 500 error - this is intentional!');
});

// Route that throws an async error
app.get('/test-async-error', async (req, res) => {
  console.log('About to throw an async test error...');
  await new Promise(resolve => setTimeout(resolve, 100));
  throw new Error('Async test error - this is also intentional!');
});

// Route that throws a custom error with additional details
app.get('/test-custom-error', (req, res) => {
  console.log('About to throw a custom error with details...');
  const error = new Error('Custom error with metadata');
  error.statusCode = 503;
  error.userMessage = 'Service temporarily unavailable';
  error.metadata = {
    service: 'payment-gateway',
    retryAfter: 30,
    requestId: 'test-' + Date.now()
  };
  throw error;
});

// Route that triggers an unhandled rejection
app.get('/test-unhandled', (req, res) => {
  console.log('Triggering unhandled rejection in 1 second...');
  res.json({ message: 'Unhandled rejection will occur in 1 second, check logs' });

  setTimeout(() => {
    Promise.reject(new Error('Unhandled promise rejection test'));
  }, 1000);
});

// Health check route
app.get('/health', (req, res) => {
  res.json({ status: 'ok', timestamp: new Date().toISOString() });
});

// Custom error handler (must be after all routes)
app.use((err, req, res, next) => {
  console.error('Error caught by Express error handler:', err);

  // Track error with Temps
  temps.trackError(err, {
    request: {
      method: req.method,
      url: req.url,
      headers: req.headers,
      ip: req.ip
    },
    custom: {
      handler: 'express-error-handler',
      timestamp: new Date().toISOString()
    }
  });

  // Send error response
  const statusCode = err.statusCode || err.status || 500;
  res.status(statusCode).json({
    error: {
      message: err.userMessage || err.message || 'Internal Server Error',
      status: statusCode,
      timestamp: new Date().toISOString(),
      ...(process.env.NODE_ENV === 'development' && { stack: err.stack })
    }
  });
});

// Handle unhandled rejections
process.on('unhandledRejection', (reason, promise) => {
  console.error('Unhandled Rejection at:', promise, 'reason:', reason);
  temps.trackError(reason, {
    type: 'unhandledRejection',
    timestamp: new Date().toISOString()
  });
});

// Handle uncaught exceptions
process.on('uncaughtException', (error) => {
  console.error('Uncaught Exception:', error);
  temps.trackError(error, {
    type: 'uncaughtException',
    timestamp: new Date().toISOString()
  });
  // Give some time for the error to be sent before exiting
  setTimeout(() => {
    process.exit(1);
  }, 1000);
});

// Start server
app.listen(port, () => {
  console.log(`\n🚀 Express Error Tracking Example Server`);
  console.log(`   Running on: http://localhost:${port}`);
  console.log(`\n📍 Test endpoints:`);
  console.log(`   GET /                 - Info page`);
  console.log(`   GET /test-error       - Throws 500 error`);
  console.log(`   GET /test-async-error - Throws async error`);
  console.log(`   GET /test-custom-error - Custom error with metadata`);
  console.log(`   GET /test-unhandled   - Unhandled promise rejection`);
  console.log(`   GET /health           - Health check`);
  console.log(`\n🔍 Temps SDK configured with:`);
  console.log(`   Base URL: http://localhost`);
  console.log(`   App ID: 1`);
  console.log(`   Debug: enabled`);
  console.log(`   Error tracking: enabled`);
  console.log(`   Request logging: enabled\n`);
});
