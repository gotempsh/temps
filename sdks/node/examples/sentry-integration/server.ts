import express, { type Request, type Response, type NextFunction } from 'express';
import * as Sentry from '@sentry/node';
import { createTransport } from '@sentry/core';

const app = express();
const port = process.env.PORT ? Number(process.env.PORT) : 3001;
const DSN_REGEX = /^(?:(\w+):)\/\/(?:(\w+)(?::(\w+)?)?@)([\w.-]+)(?::(\d+))?\/(.+)/;

const sentryDsn = 'https://b190cf474c1306fc10c8985353021cdd0da418a081f4d85d731854527fd7e7e0@app.tempslocal.kfs.es/1';
const match = sentryDsn.match(DSN_REGEX);
if (match) {
  const [, protocol, publicKey, host, projectId] = match;
  console.log(protocol, publicKey, host, projectId);
}

// Custom transport to log what's being sent to the API
function makeFetchTransport(options: any) {
  function makeRequest(request: any) {
    // console.log('🚀 Sending to Sentry API:', {
    //   url: options.url,
    //   method: 'POST',
    //   headers: options.headers,
    //   body: request.body,
    //   timestamp: new Date().toISOString(),
    // });

    const requestOptions: RequestInit = {
      body: request.body,
      method: 'POST',
      referrerPolicy: 'origin',
      headers: options.headers,
      ...options.fetchOptions,
    };

    return fetch(options.url, requestOptions).then(async response => {
      // Read response body for logging
      let responseBody = '';
      try {
        responseBody = await response.text();
        // console.log('📥 Response from Sentry API:', {
        //   status: response.status,
        //   statusText: response.statusText,
        //   headers: {
        //     'x-sentry-rate-limits': response.headers.get('X-Sentry-Rate-Limits'),
        //     'retry-after': response.headers.get('Retry-After'),
        //     'content-type': response.headers.get('Content-Type'),
        //   },
        //   body: responseBody,
        //   timestamp: new Date().toISOString(),
        // });
      } catch (bodyError) {
        console.log('📥 Response from Sentry API (no body):', {
          status: response.status,
          statusText: response.statusText,
          headers: {
            'x-sentry-rate-limits': response.headers.get('X-Sentry-Rate-Limits'),
            'retry-after': response.headers.get('Retry-After'),
            'content-type': response.headers.get('Content-Type'),
          },
          timestamp: new Date().toISOString(),
        });
        console.error('❌ Error reading response body:', bodyError instanceof Error ? bodyError.message : String(bodyError));
      }

      return {
        statusCode: response.status,
        headers: {
          'x-sentry-rate-limits': response.headers.get('X-Sentry-Rate-Limits'),
          'retry-after': response.headers.get('Retry-After'),
        },
      };
    }).catch(error => {
      console.error('❌ Error sending to Sentry API:', {
        error: error.message,
        url: options.url,
        timestamp: new Date().toISOString(),
      });
      throw error;
    });
  }

  return createTransport(options, makeRequest);
}

// Initialize Sentry with custom endpoint
// The DSN format is: {protocol}://{public_key}@{host}/{project_id}
// We'll construct it from your credentials
Sentry.init({
  dsn: sentryDsn,
  transport: makeFetchTransport,
  // Custom transport options to ensure it uses your endpoint
  // transportOptions: {
  //   // Custom headers if needed for authentication
  //   headers: {
  //     'Authorization':
  //       'Basic ' +
  //       Buffer.from(
  //         'oS4BvLtdW0IaoLmWrgLpLBAleXQLPfyd9S4siuF_-kk:DD763bN8ff_b71lGqn6lRtM5nv6VJGxJ6LAGtpPHsZ0'
  //       ).toString('base64'),
  //   },
  // },

  // Performance Monitoring
  tracesSampleRate: 1.0, // Capture 100% of transactions for testing

  // Session Tracking
  autoSessionTracking: true,

  // Release Tracking
  release: 'sentry-example@1.0.0',

  // Environment
  environment: process.env.NODE_ENV || 'development',

  // Integrations
  integrations: [
    // Enable HTTP calls tracing
    new Sentry.Integrations.Http({ tracing: false }),
    // Enable Express.js middleware tracing
    new Sentry.Integrations.Express({ app }),
  ],

  // Set sampling rate for profiling
  profilesSampleRate: 1.0,

  // Debug mode to see what's being sent
  debug: true,

  // Before send hook to log what's being sent
  beforeSend: (event: Sentry.Event, hint?: Sentry.EventHint) => {
    console.log('Sending event to Sentry:', {
      event_id: event.event_id,
      message: event.message,
      exception: event.exception,
      level: event.level,
      timestamp: event.timestamp,
    });
    return event;
  },

  // Before send transaction hook
  beforeSendTransaction: (event, hint) => {
    console.log('Sending transaction to Sentry:', {
      event_id: event.event_id,
      transaction: (event as any).transaction,
      type: event.type,
    });
    return event;
  },
});

// RequestHandler creates a separate execution context, so that all
// transactions/spans/breadcrumbs are isolated across requests
app.use(Sentry.Handlers.requestHandler() as express.RequestHandler);

// TracingHandler creates a trace for every incoming request
// app.use(Sentry.Handlers.tracingHandler() as express.RequestHandler);

// Parse JSON bodies
app.use(express.json());

// Root route
app.get('/', (req: Request, res: Response) => {
  res.json({
    message: 'Sentry Integration Example',
    routes: {
      '/': 'This info page',
      '/test-error': 'Throws a basic error',
      '/test-warning': 'Sends a warning message',
      '/test-transaction': 'Creates a performance transaction',
      '/test-breadcrumb': 'Adds breadcrumbs then errors',
      '/test-user-context': 'Error with user context',
      '/test-tags': 'Error with custom tags',
      '/test-500': 'Returns 500 status',
      '/debug-sentry': 'Triggers Sentry debug test',
    },
    sentry: {
      dsn: 'http://localhost/1',
      environment: Sentry.getCurrentHub().getClient()?.getOptions().environment,
      release: Sentry.getCurrentHub().getClient()?.getOptions().release,
    },
  });
});

// Basic error test
app.get('/test-error', (req: Request, res: Response) => {
  console.log('Testing basic error...');
  throw new Error('Test error from Sentry integration example!');
});

// Warning message test
app.get('/test-warning', (req: Request, res: Response) => {
  console.log('Sending warning to Sentry...');
  Sentry.captureMessage('Test warning message', 'warning');
  res.json({ message: 'Warning sent to Sentry' });
});

// Performance transaction test
app.get('/test-transaction', async (req: Request, res: Response) => {
  const transaction = Sentry.startTransaction({
    op: 'test-performance',
    name: 'Test Performance Transaction',
  });

  Sentry.getCurrentHub().configureScope((scope: Sentry.Scope) => scope.setSpan(transaction));

  const span1 = transaction.startChild({
    op: 'db.query',
    description: 'SELECT * FROM users',
  });

  // Simulate database query
  await new Promise((resolve) => setTimeout(resolve, 100));
  span1.finish();

  const span2 = transaction.startChild({
    op: 'http.request',
    description: 'GET /api/external',
  });

  // Simulate HTTP request
  await new Promise((resolve) => setTimeout(resolve, 200));
  span2.finish();

  transaction.finish();

  res.json({ message: 'Performance transaction recorded' });
});

// Breadcrumb test
app.get('/test-breadcrumb', (req: Request, res: Response) => {
  console.log('Testing breadcrumbs...');

  Sentry.addBreadcrumb({
    message: 'User clicked test-breadcrumb',
    level: 'info',
    category: 'user-action',
    timestamp: Date.now() / 1000,
  });

  Sentry.addBreadcrumb({
    message: 'Loading user data',
    level: 'info',
    category: 'data-fetch',
    data: {
      userId: '12345',
      action: 'fetch',
    },
  });

  Sentry.addBreadcrumb({
    message: 'User data validation failed',
    level: 'warning',
    category: 'validation',
  });

  // Now throw an error - breadcrumbs will be attached
  throw new Error('Error after breadcrumbs were added');
});

// User context test
app.get('/test-user-context', (req: Request, res: Response) => {
  console.log('Testing user context...');

  Sentry.setUser({
    id: '12345',
    username: 'testuser',
    email: 'test@example.com',
    ip_address: req.ip,
  });

  Sentry.setContext('additional_info', {
    subscription: 'premium',
    signup_date: '2024-01-15',
    last_login: new Date().toISOString(),
  });

  throw new Error('Error with user context attached');
});

// Custom tags test
app.get('/test-tags', (req: Request, res: Response) => {
  console.log('Testing custom tags...');

  Sentry.setTag('module', 'payment');
  Sentry.setTag('customer_type', 'premium');
  Sentry.setTag('feature_flag', 'new_checkout_enabled');
  Sentry.setTag('api_version', 'v2');

  Sentry.setExtra('request_details', {
    user_agent: req.get('user-agent'),
    referer: req.get('referer'),
    query_params: req.query,
  });

  // Extend Error type for custom properties
  interface CustomError extends Error {
    code?: string;
    statusCode?: number;
  }
  const error: CustomError = new Error('Error with custom tags and extra data');
  error.code = 'PAYMENT_FAILED';
  error.statusCode = 402;

  throw error;
});

// Return 500 status without throwing
app.get('/test-500', (req: Request, res: Response) => {
  console.log('Returning 500 status...');

  // Log to Sentry without throwing
  Sentry.captureException(new Error('500 Internal Server Error (handled)'), {
    tags: {
      handled: true,
      endpoint: '/test-500',
    },
    level: 'error',
  });

  res.status(500).json({
    error: 'Internal Server Error',
    message: 'This is a handled 500 error',
  });
});

// Sentry debug test endpoint
app.get('/debug-sentry', (req: Request, res: Response) => {
  console.log('Running Sentry debug test...');
  Sentry.captureMessage('Sentry debug test message', 'debug');
  res.json({
    message: 'Debug message sent',
    sentry_configured: !!Sentry.getCurrentHub().getClient(),
  });
});

// Health check
app.get('/health', (req: Request, res: Response) => {
  const client = Sentry.getCurrentHub().getClient();
  res.json({
    status: 'ok',
    timestamp: new Date().toISOString(),
    sentry: {
      enabled: !!client,
      dsn: client?.getDsn()?.toString(),
      environment: client?.getOptions().environment,
    },
  });
});

// The error handler must be registered before any other error middleware and after all controllers
app.use(
  Sentry.Handlers.errorHandler({
    shouldHandleError(error: unknown) {
      // Capture all errors
      return true;
    },
  }) as express.ErrorRequestHandler
);

// Custom error handler (after Sentry's)
app.use(
  (
    err: any,
    req: Request,
    res: Response,
    next: NextFunction // eslint-disable-line @typescript-eslint/no-unused-vars
  ) => {
    console.error('Error caught by Express:', err);

    const statusCode = err.statusCode || err.status || 500;
    res.status(statusCode).json({
      error: {
        message: err.message || 'Internal Server Error',
        status: statusCode,
        timestamp: new Date().toISOString(),
        sentry_id: (res as any).sentry,
        ...(process.env.NODE_ENV === 'development' && { stack: err.stack }),
      },
    });
  }
);

// Start server
app.listen(port, () => {
  console.log(`\n🚀 Sentry Integration Example Server`);
  console.log(`   Running on: http://localhost:${port}`);
  console.log(`\n📍 Test endpoints:`);
  console.log(`   GET /                   - Info page`);
  console.log(`   GET /test-error         - Basic error`);
  console.log(`   GET /test-warning       - Warning message`);
  console.log(`   GET /test-transaction   - Performance monitoring`);
  console.log(`   GET /test-breadcrumb    - Error with breadcrumbs`);
  console.log(`   GET /test-user-context  - Error with user context`);
  console.log(`   GET /test-tags          - Error with custom tags`);
  console.log(`   GET /test-500           - Handled 500 error`);
  console.log(`   GET /debug-sentry       - Debug test`);
  console.log(`   GET /health             - Health check`);
  console.log(`\n🔍 Sentry configured with:`);
  console.log(`   DSN: http://localhost/1`);
  console.log(`   Environment: ${process.env.NODE_ENV || 'development'}`);
  console.log(`   Debug: enabled`);
  console.log(`   Traces Sample Rate: 100%`);
  console.log(`   Profiles Sample Rate: 100%\n`);
});
