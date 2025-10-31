import { ErrorTracking } from '../dist/index.js';

// Initialize error tracking with your DSN
ErrorTracking.init({
  dsn: 'https://your-public-key@your-sentry-server.com/project-id',
  environment: 'development',
  release: '1.0.0',
  sampleRate: 1.0, // Capture 100% of errors
  debug: true, // Enable debug mode to log to console instead of sending
  beforeSend: (event) => {
    // Optionally modify events before sending
    console.log('About to send event:', event.event_id);
    return event;
  }
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

ErrorTracking.addBreadcrumb({
  message: 'Payment method selected',
  category: 'user-action',
  data: { method: 'credit-card' }
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
    extra: {
      raw_input: 'invalid json',
      timestamp: new Date().toISOString()
    }
  });
}

// Capture a custom error
class PaymentError extends Error {
  constructor(message, code) {
    super(message);
    this.name = 'PaymentError';
    this.code = code;
  }
}

const paymentError = new PaymentError('Payment declined', 'CARD_DECLINED');
ErrorTracking.captureException(paymentError);

// Work with scoped context
ErrorTracking.withScope((scope) => {
  scope.setTag('transaction_id', 'txn_123456');
  scope.setLevel('warning');

  // This error will have the transaction_id tag
  ErrorTracking.captureMessage('Payment processing delayed');
});

// The transaction_id tag is not applied here
ErrorTracking.captureMessage('Another message without transaction context');

// Configure global scope
ErrorTracking.configureScope((scope) => {
  scope.setContext('payment', {
    provider: 'Stripe',
    api_version: '2023-10-16'
  });
});

// Simulate async error handling
async function processPaymentAsync() {
  try {
    // Simulate async operation
    await new Promise((resolve, reject) => {
      setTimeout(() => reject(new Error('Network timeout')), 100);
    });
  } catch (error) {
    ErrorTracking.captureException(error, {
      tags: { async: 'true' }
    });
  }
}

processPaymentAsync();

// Clean up when done
setTimeout(async () => {
  console.log('Flushing remaining events...');
  await ErrorTracking.flush(2000);

  console.log('Closing error tracking client...');
  await ErrorTracking.close(2000);

  console.log('Done!');
}, 1000);
