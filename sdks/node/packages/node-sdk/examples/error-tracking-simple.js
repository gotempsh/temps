import * as ErrorTracking from '../dist/errors/index.js';

// Initialize error tracking with your DSN
ErrorTracking.init({
  dsn: 'https://your-public-key@your-sentry-server.com/project-id',
  environment: 'development',
  release: '1.0.0',
  sampleRate: 1.0, // Capture 100% of errors
  debug: true, // Enable debug mode to log to console instead of sending
});

// Set user context
ErrorTracking.setUser({
  id: '12345',
  username: 'john_doe',
  email: 'john@example.com'
});

// Set global tags
ErrorTracking.setTags({
  module: 'payment',
  version: '2.0.0'
});

// Add breadcrumbs for debugging
ErrorTracking.addBreadcrumb({
  message: 'User navigated to checkout',
  category: 'navigation',
  level: 'info'
});

// Capture a simple message
ErrorTracking.captureMessage('User completed checkout successfully', 'info');

// Capture an error with stack trace
try {
  // Simulate an error
  const data = JSON.parse('invalid json');
} catch (error) {
  // Capture the exception with additional context
  ErrorTracking.captureException(error, {
    tags: {
      error_type: 'json_parse',
      input_source: 'api_response'
    },
    level: 'error',
  });
}

console.log('Error tracking example completed!');
