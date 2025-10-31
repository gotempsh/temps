import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import { ErrorTrackingClient } from '../client';
import type { ErrorTrackingOptions, Event } from '../types';

const createMockTransport = () => ({
  sendEvent: vi.fn().mockResolvedValue(undefined),
});

vi.mock('../transport', () => ({
  createTransportFromDsn: vi.fn(() => createMockTransport()),
}));

describe('Automatic Stack Trace Handling', () => {
  let client: ErrorTrackingClient;
  let mockTransport: any;
  let originalProcess: any;

  const defaultOptions: ErrorTrackingOptions = {
    dsn: 'https://test@sentry.io/42',
    debug: false,
    attachStacktrace: true,
  };

  beforeEach(() => {
    originalProcess = {
      on: process.on,
      exit: process.exit,
    };

    process.on = vi.fn();
    process.exit = vi.fn();

    client = new ErrorTrackingClient(defaultOptions);
    mockTransport = (client as any).transport;
  });

  afterEach(() => {
    process.on = originalProcess.on;
    process.exit = originalProcess.exit;
    vi.clearAllMocks();
  });

  describe('Stack Trace Extraction', () => {
    it('should automatically extract and attach stack traces from Error objects', () => {
      const error = new Error('Test error with stack trace');
      const eventId = client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          event_id: eventId,
          exception: {
            values: [
              expect.objectContaining({
                type: 'Error',
                value: 'Test error with stack trace',
                stacktrace: expect.objectContaining({
                  frames: expect.any(Array),
                }),
                mechanism: {
                  type: 'generic',
                  handled: true,
                },
              }),
            ],
          },
        })
      );

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      expect(frames.length).toBeGreaterThan(0);

      frames.forEach((frame: any) => {
        if (frame.filename && !frame.filename.includes('node_modules') && !frame.filename.includes('node:') && !frame.filename.includes('vitest')) {
          expect(frame.in_app).toBe(true);
        }
      });
    });

    it('should handle TypeError with stack traces', () => {
      const error = new TypeError('Cannot read property of undefined');
      error.stack = `TypeError: Cannot read property of undefined
    at someFunction (/app/src/module.js:42:15)
    at anotherFunction (/app/src/index.js:10:5)
    at Object.<anonymous> (/app/test.js:5:1)`;

      client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: {
            values: [
              expect.objectContaining({
                type: 'TypeError',
                value: 'Cannot read property of undefined',
                stacktrace: {
                  frames: expect.arrayContaining([
                    expect.objectContaining({
                      filename: '/app/test.js',
                      function: 'Object.<anonymous>',
                      lineno: 5,
                      colno: 1,
                      in_app: true,
                    }),
                    expect.objectContaining({
                      filename: '/app/src/index.js',
                      function: 'anotherFunction',
                      lineno: 10,
                      colno: 5,
                      in_app: true,
                    }),
                    expect.objectContaining({
                      filename: '/app/src/module.js',
                      function: 'someFunction',
                      lineno: 42,
                      colno: 15,
                      in_app: true,
                    }),
                  ]),
                },
              }),
            ],
          },
        })
      );
    });

    it('should handle ReferenceError with complex stack traces', () => {
      const error = new ReferenceError('variable is not defined');
      error.stack = `ReferenceError: variable is not defined
    at eval (eval at createFunction (/app/utils/factory.js:100:10), <anonymous>:1:1)
    at createFunction (/app/utils/factory.js:100:10)
    at processData (/app/src/processor.js:50:25)
    at async Promise.all (index 0)
    at async main (/app/index.js:20:5)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const exception = sentEvent.exception.values[0];

      expect(exception.type).toBe('ReferenceError');
      expect(exception.value).toBe('variable is not defined');
      expect(exception.stacktrace.frames).toBeDefined();
      expect(exception.stacktrace.frames.length).toBeGreaterThan(0);

      const evalFrame = exception.stacktrace.frames.find(
        (f: any) => f.function && f.function.includes('eval')
      );
      expect(evalFrame).toBeDefined();
    });

    it('should handle SyntaxError with minimal stack', () => {
      const error = new SyntaxError('Unexpected token');
      error.stack = `SyntaxError: Unexpected token
    at /app/broken.js:1:1`;

      client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: {
            values: [
              expect.objectContaining({
                type: 'SyntaxError',
                value: 'Unexpected token',
                stacktrace: {
                  frames: [
                    expect.objectContaining({
                      filename: '/app/broken.js',
                      function: '<anonymous>',
                      lineno: 1,
                      colno: 1,
                      in_app: true,
                    }),
                  ],
                },
              }),
            ],
          },
        })
      );
    });

    it('should handle custom Error classes with inheritance', () => {
      class CustomError extends Error {
        constructor(message: string) {
          super(message);
          this.name = 'CustomError';
        }
      }

      const error = new CustomError('Custom error occurred');
      error.stack = `CustomError: Custom error occurred
    at CustomClass.method (/app/src/custom.js:25:10)
    at handler (/app/src/handler.js:15:5)`;

      client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: {
            values: [
              expect.objectContaining({
                type: 'CustomError',
                value: 'Custom error occurred',
                stacktrace: expect.objectContaining({
                  frames: expect.any(Array),
                }),
              }),
            ],
          },
        })
      );
    });
  });

  describe('Stack Trace Processing', () => {
    it('should correctly mark node_modules frames as not in_app', () => {
      const error = new Error('Mixed stack trace');
      error.stack = `Error: Mixed stack trace
    at userFunction (/app/src/user.js:10:5)
    at libraryFunction (/app/node_modules/library/index.js:50:15)
    at anotherUserFunction (/app/src/another.js:20:10)
    at expressMiddleware (/app/node_modules/express/lib/router.js:100:5)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      const nodeModulesFrame = frames.find(
        (f: any) => f.filename && f.filename.includes('node_modules')
      );
      const appFrame = frames.find(
        (f: any) => f.filename && f.filename.includes('/app/src/')
      );

      expect(nodeModulesFrame?.in_app).toBe(false);
      expect(appFrame?.in_app).toBe(true);
    });

    it('should handle Node.js internal frames', () => {
      const error = new Error('Internal frames');
      error.stack = `Error: Internal frames
    at userCode (/app/index.js:5:10)
    at Module._compile (node:internal/modules/cjs/loader:1120:14)
    at Module._extensions..js (node:internal/modules/cjs/loader:1174:10)
    at Module.load (node:internal/modules/cjs/loader:988:32)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      const internalFrames = frames.filter(
        (f: any) => f.filename && f.filename.startsWith('node:')
      );

      internalFrames.forEach((frame: any) => {
        expect(frame.in_app).toBe(false);
      });
    });

    it('should preserve frame order (reversed for display)', () => {
      const error = new Error('Frame order test');
      error.stack = `Error: Frame order test
    at first (/app/first.js:1:1)
    at second (/app/second.js:2:2)
    at third (/app/third.js:3:3)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      expect(frames).toHaveLength(3);

      expect(frames[0].filename).toBe('/app/third.js');
      expect(frames[0].lineno).toBe(3);

      expect(frames[1].filename).toBe('/app/second.js');
      expect(frames[1].lineno).toBe(2);

      expect(frames[2].filename).toBe('/app/first.js');
      expect(frames[2].lineno).toBe(1);
    });

    it('should handle very long stack traces', () => {
      const lines = ['Error: Deep recursion'];
      for (let i = 0; i < 100; i++) {
        lines.push(`    at recursiveFunction (/app/recursive.js:${i}:1)`);
      }

      const error = new Error('Deep recursion');
      error.stack = lines.join('\\n');

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      expect(frames.length).toBeLessThanOrEqual(50);
    });
  });

  describe('Error Context Enrichment', () => {
    it('should include stack trace even for errors caught in try-catch', () => {
      const error = new Error('Caught error');

      client.captureException(error, {
        tags: { handled: 'true' },
        level: 'warning',
      });

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];

      expect(sentEvent.tags).toEqual({ handled: 'true' });
      expect(sentEvent.exception).toBeDefined();
      expect(sentEvent.exception.values).toBeDefined();
      expect(sentEvent.exception.values[0].stacktrace).toBeDefined();
      expect(sentEvent.exception.values[0].stacktrace.frames).toBeInstanceOf(Array);
      expect(sentEvent.exception.values[0].mechanism).toEqual({
        type: 'generic',
        handled: true,
      });
    });

    it('should handle async/await stack traces', () => {
      const error = new Error('Async error');
      error.stack = `Error: Async error
    at async fetchData (/app/src/api.js:25:11)
    at async processRequest (/app/src/handler.js:10:20)
    at async Promise.all (index 0)
    at async main (/app/index.js:5:3)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      const asyncFrames = frames.filter(
        (f: any) => f.function && f.function.includes('async')
      );

      expect(asyncFrames.length).toBeGreaterThan(0);
    });

    it('should handle Promise rejection stack traces', () => {
      const error = new Error('Promise rejection');
      error.stack = `Error: Promise rejection
    at Promise.then (/app/promise.js:10:5)
    at processTicksAndRejections (node:internal/process/task_queues:95:5)`;

      client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: {
            values: [
              expect.objectContaining({
                type: 'Error',
                value: 'Promise rejection',
                stacktrace: expect.objectContaining({
                  frames: expect.any(Array),
                }),
              }),
            ],
          },
        })
      );
    });
  });

  describe('Stack Trace Configuration', () => {
    it('should not attach stack traces when attachStacktrace is false', () => {
      const clientNoStack = new ErrorTrackingClient({
        dsn: 'https://test@sentry.io/42',
        attachStacktrace: false,
      });
      const transport = (clientNoStack as any).transport;

      const errorWithoutMessage = { code: 'ERR_001' };
      clientNoStack.captureException(errorWithoutMessage);

      expect(transport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          message: expect.any(String),
        })
      );

      const sentEvent = transport.sendEvent.mock.calls[0][0];
      expect(sentEvent.exception).toBeUndefined();
    });

    it('should handle errors without stack property gracefully', () => {
      const error = new Error('No stack');
      delete (error as any).stack;

      client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: {
            values: [
              expect.objectContaining({
                type: 'Error',
                value: 'No stack',
                stacktrace: {
                  frames: [],
                },
              }),
            ],
          },
        })
      );
    });

    it('should handle malformed stack traces', () => {
      const error = new Error('Malformed');
      error.stack = `Error: Malformed
    this is not a valid stack frame
    at validFrame (/app/valid.js:1:1)
    another invalid line`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      const validFrame = frames.find((f: any) => f.filename === '/app/valid.js');
      expect(validFrame).toBeDefined();
      expect(validFrame?.lineno).toBe(1);
    });
  });

  describe('Global Error Handlers', () => {
    it('should capture uncaught exceptions with stack traces', () => {
      const mockProcessOn = process.on as vi.Mock;
      const uncaughtHandler = mockProcessOn.mock.calls.find(
        call => call[0] === 'uncaughtException'
      )?.[1];

      expect(uncaughtHandler).toBeDefined();

      const uncaughtError = new Error('Uncaught exception');
      uncaughtError.stack = `Error: Uncaught exception
    at fatal (/app/fatal.js:10:5)`;

      uncaughtHandler(uncaughtError);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];

      expect(sentEvent.level).toBe('fatal');
      expect(sentEvent.tags).toEqual({ handled: 'false' });
      expect(sentEvent.exception).toBeDefined();
      expect(sentEvent.exception.values[0].type).toBe('Error');
      expect(sentEvent.exception.values[0].value).toBe('Uncaught exception');
      expect(sentEvent.exception.values[0].stacktrace.frames).toBeDefined();

      const fatalFrame = sentEvent.exception.values[0].stacktrace.frames.find(
        (f: any) => f.filename === '/app/fatal.js'
      );
      expect(fatalFrame).toBeDefined();
      expect(fatalFrame.function).toBe('fatal');
      expect(fatalFrame.lineno).toBe(10);
      expect(fatalFrame.colno).toBe(5);

      expect(process.exit).toHaveBeenCalledWith(1);
    });

    it('should capture unhandled rejections with stack traces', () => {
      const mockProcessOn = process.on as vi.Mock;
      const rejectionHandler = mockProcessOn.mock.calls.find(
        call => call[0] === 'unhandledRejection'
      )?.[1];

      expect(rejectionHandler).toBeDefined();

      const rejectionError = new Error('Unhandled rejection');
      rejectionError.stack = `Error: Unhandled rejection
    at asyncOperation (/app/async.js:20:10)`;

      rejectionHandler(rejectionError);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          level: 'error',
          tags: { handled: 'false', type: 'unhandledRejection' },
          exception: {
            values: [
              expect.objectContaining({
                stacktrace: expect.objectContaining({
                  frames: expect.arrayContaining([
                    expect.objectContaining({
                      filename: '/app/async.js',
                      function: 'asyncOperation',
                    }),
                  ]),
                }),
              }),
            ],
          },
        })
      );
    });

    it('should handle non-Error unhandled rejections', () => {
      const mockProcessOn = process.on as vi.Mock;
      const rejectionHandler = mockProcessOn.mock.calls.find(
        call => call[0] === 'unhandledRejection'
      )?.[1];

      rejectionHandler('String rejection reason');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          level: 'error',
          tags: { handled: 'false', type: 'unhandledRejection' },
          exception: expect.objectContaining({
            values: expect.arrayContaining([
              expect.objectContaining({
                type: 'Error',
                value: 'String rejection reason',
              }),
            ]),
          }),
        })
      );
    });
  });

  describe('Stack Trace with Capture Context', () => {
    it('should preserve stack traces when additional context is provided', () => {
      const error = new Error('Context error');
      error.stack = `Error: Context error
    at contextFunction (/app/context.js:15:10)`;

      client.captureException(error, {
        tags: { module: 'auth' },
        user: { id: 'user123' },
        extra: { requestId: 'req-456' },
        contexts: {
          runtime: { name: 'node', version: '16.0.0' },
        },
      });

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          tags: { module: 'auth' },
          user: { id: 'user123' },
          extra: { requestId: 'req-456' },
          contexts: {
            runtime: { name: 'node', version: '16.0.0' },
          },
          exception: {
            values: [
              expect.objectContaining({
                stacktrace: expect.objectContaining({
                  frames: expect.arrayContaining([
                    expect.objectContaining({
                      filename: '/app/context.js',
                      function: 'contextFunction',
                      lineno: 15,
                      colno: 10,
                    }),
                  ]),
                }),
              }),
            ],
          },
        })
      );
    });

    it('should apply beforeSend while preserving stack traces', () => {
      const beforeSend = vi.fn((event: Event) => {
        if (event.exception?.values?.[0]?.stacktrace?.frames) {
          event.exception.values[0].stacktrace.frames =
            event.exception.values[0].stacktrace.frames.filter(
              (f: any) => f.in_app === true
            );
        }
        return event;
      });

      const clientWithHook = new ErrorTrackingClient({
        dsn: 'https://test@sentry.io/42',
        beforeSend,
      });
      const transport = (clientWithHook as any).transport;

      const error = new Error('Filtered stack');
      error.stack = `Error: Filtered stack
    at userCode (/app/user.js:10:5)
    at libraryCode (/app/node_modules/lib/index.js:50:10)
    at moreUserCode (/app/user2.js:20:5)`;

      clientWithHook.captureException(error);

      expect(beforeSend).toHaveBeenCalled();

      const sentEvent = transport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      expect(frames.every((f: any) => f.in_app === true)).toBe(true);
      expect(frames.every((f: any) => !f.filename.includes('node_modules'))).toBe(true);
    });
  });

  describe('Edge Cases', () => {
    it('should handle circular references in error objects', () => {
      const error: any = new Error('Circular');
      error.circular = error;
      error.stack = `Error: Circular
    at test (/app/test.js:1:1)`;

      expect(() => client.captureException(error)).not.toThrow();

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: expect.objectContaining({
            values: expect.arrayContaining([
              expect.objectContaining({
                type: 'Error',
                value: 'Circular',
              }),
            ]),
          }),
        })
      );
    });

    it('should handle errors with very long messages', () => {
      const longMessage = 'A'.repeat(10000);
      const error = new Error(longMessage);
      error.stack = `Error: ${longMessage}
    at test (/app/test.js:1:1)`;

      client.captureException(error);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: {
            values: [
              expect.objectContaining({
                value: longMessage,
                stacktrace: expect.objectContaining({
                  frames: expect.any(Array),
                }),
              }),
            ],
          },
        })
      );
    });

    it('should handle stack traces with special characters', () => {
      const error = new Error('Special chars');
      error.stack = `Error: Special chars
    at <anonymous> (/app/src/[id]/page.js:10:5)
    at Function.$create (/app/src/\${utils}.js:20:10)
    at Object.<computed> (/app/src/@namespace/file.js:30:15)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      expect(frames).toBeDefined();
      expect(frames.length).toBeGreaterThan(0);

      const specialFrame = frames.find(
        (f: any) => f.filename && f.filename.includes('[id]')
      );
      expect(specialFrame).toBeDefined();
    });

    it('should handle stack traces from eval code', () => {
      const error = new Error('Eval error');
      error.stack = `Error: Eval error
    at eval (eval at compileFunction (/app/compiler.js:50:10), <anonymous>:2:5)
    at compileFunction (/app/compiler.js:50:10)
    at run (/app/runner.js:10:5)`;

      client.captureException(error);

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      const frames = sentEvent.exception.values[0].stacktrace.frames;

      const evalFrame = frames.find(
        (f: any) => f.function && f.function.includes('eval')
      );

      expect(evalFrame).toBeDefined();
      expect(evalFrame?.filename).toBe('/app/compiler.js');
      expect(evalFrame?.lineno).toBe(50);
      expect(evalFrame?.colno).toBe(10);
    });

    it('should handle aggregate errors with multiple stack traces', () => {
      const errors = [
        new Error('First error'),
        new TypeError('Second error'),
      ];

      errors[0].stack = `Error: First error
    at first (/app/first.js:1:1)`;

      errors[1].stack = `TypeError: Second error
    at second (/app/second.js:2:2)`;

      const aggregateError: any = new Error('Multiple errors occurred');
      aggregateError.errors = errors;

      client.captureException(aggregateError);

      expect(mockTransport.sendEvent).toHaveBeenCalled();

      const sentEvent = mockTransport.sendEvent.mock.calls[0][0];
      expect(sentEvent.exception).toBeDefined();
    });
  });
});
