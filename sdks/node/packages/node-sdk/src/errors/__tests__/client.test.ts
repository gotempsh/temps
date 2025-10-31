import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import { ErrorTrackingClient } from '../client';
import type { ErrorTrackingOptions, Event } from '../types';

// Mock transport
const createMockTransport = () => ({
  sendEvent: vi.fn().mockResolvedValue(undefined),
});

vi.mock('../transport', () => ({
  createTransportFromDsn: vi.fn(() => createMockTransport()),
  HttpTransport: vi.fn(),
  ConsoleTransport: vi.fn(),
  parseDsn: vi.fn((dsn) => ({
    protocol: 'https',
    host: 'sentry.io',
    projectId: '42',
    publicKey: 'test',
  })),
}));

describe('ErrorTrackingClient', () => {
  let client: ErrorTrackingClient;
  let mockTransport: any;
  let originalProcess: any;

  const defaultOptions: ErrorTrackingOptions = {
    dsn: 'https://test@sentry.io/42',
    debug: false,
  };

  beforeEach(() => {
    // Store original process handlers
    originalProcess = {
      on: process.on,
      exit: process.exit,
    };

    // Mock process.on to prevent actual error handlers
    process.on = vi.fn();
    process.exit = vi.fn();

    // Create client and get mock transport
    client = new ErrorTrackingClient(defaultOptions);
    mockTransport = (client as any).transport;
  });

  afterEach(() => {
    // Restore process handlers
    process.on = originalProcess.on;
    process.exit = originalProcess.exit;
    vi.clearAllMocks();
  });

  describe('constructor', () => {
    it('should initialize with default options', () => {
      const client = new ErrorTrackingClient({
        dsn: 'https://test@sentry.io/42',
      });

      expect(client).toBeDefined();
      expect((client as any).options.environment).toBe('production');
      expect((client as any).options.sampleRate).toBe(1.0);
      expect((client as any).options.maxBreadcrumbs).toBe(100);
      expect((client as any).options.attachStacktrace).toBe(true);
    });

    it('should merge custom options with defaults', () => {
      const client = new ErrorTrackingClient({
        dsn: 'https://test@sentry.io/42',
        environment: 'staging',
        sampleRate: 0.5,
        debug: true,
      });

      expect((client as any).options.environment).toBe('staging');
      expect((client as any).options.sampleRate).toBe(0.5);
      expect((client as any).options.debug).toBe(true);
    });

    it('should setup integrations if provided', () => {
      const mockIntegration = {
        name: 'TestIntegration',
        setupOnce: vi.fn(),
      };

      new ErrorTrackingClient({
        dsn: 'https://test@sentry.io/42',
        integrations: [mockIntegration],
      });

      expect(mockIntegration.setupOnce).toHaveBeenCalled();
    });

    it('should setup global error handlers', () => {
      new ErrorTrackingClient(defaultOptions);

      expect(process.on).toHaveBeenCalledWith('uncaughtException', expect.any(Function));
      expect(process.on).toHaveBeenCalledWith('unhandledRejection', expect.any(Function));
    });
  });

  describe('captureException', () => {
    it('should capture Error with stack trace', () => {
      const error = new Error('Test error');
      const eventId = client.captureException(error);

      expect(eventId).toMatch(/^[a-f0-9]{32}$/);
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          event_id: eventId,
          level: 'error',
          platform: 'node',
          exception: expect.objectContaining({
            values: expect.arrayContaining([
              expect.objectContaining({
                type: 'Error',
                value: 'Test error',
              }),
            ]),
          }),
        })
      );
    });

    it('should capture non-Error objects', () => {
      const errorObj = { message: 'Custom error', code: 500 };
      const eventId = client.captureException(errorObj);

      expect(eventId).toBeTruthy();
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          message: 'Custom error', // extractErrorMessage extracts the message property
        })
      );
    });

    it('should capture strings as messages', () => {
      const eventId = client.captureException('String error');

      expect(eventId).toBeTruthy();
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          message: 'String error',
        })
      );
    });

    it('should apply capture context', () => {
      const error = new Error('Test');
      const eventId = client.captureException(error, {
        tags: { custom: 'tag' },
        user: { id: '123' },
        level: 'warning',
      });

      expect(eventId).toBeTruthy();
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          tags: expect.objectContaining({ custom: 'tag' }),
          user: expect.objectContaining({ id: '123' }),
        })
      );
    });

    it('should respect sample rate', () => {
      // Use Math.random mock to control sampling
      const randomSpy = vi.spyOn(Math, 'random');
      randomSpy.mockReturnValue(0.5); // 50% chance

      const client = new ErrorTrackingClient({
        ...defaultOptions,
        sampleRate: 0.3, // 30% sample rate
      });
      const transport = (client as any).transport;

      client.captureException(new Error('Test'));

      // 0.5 > 0.3, so event should be dropped
      expect(transport.sendEvent).not.toHaveBeenCalled();

      randomSpy.mockRestore();
    });

    it('should ignore errors based on ignoreErrors patterns', () => {
      const client = new ErrorTrackingClient({
        ...defaultOptions,
        ignoreErrors: ['NetworkError', /timeout/i],
      });
      const transport = (client as any).transport;

      client.captureException(new Error('NetworkError occurred'));
      client.captureException(new Error('Request timeout'));
      client.captureException(new Error('Other error'));

      expect(transport.sendEvent).toHaveBeenCalledTimes(1);
      expect(transport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          exception: expect.objectContaining({
            values: expect.arrayContaining([
              expect.objectContaining({
                value: 'Other error',
              }),
            ]),
          }),
        })
      );
    });

    it('should not capture when disabled', () => {
      client.setEnabled(false);
      client.captureException(new Error('Test'));

      expect(mockTransport.sendEvent).not.toHaveBeenCalled();
    });

    it('should apply beforeSend hook', () => {
      const beforeSend = vi.fn((event) => ({
        ...event,
        tags: { ...event.tags, modified: 'true' },
      }));

      const client = new ErrorTrackingClient({
        ...defaultOptions,
        beforeSend,
      });
      const transport = (client as any).transport;

      client.captureException(new Error('Test'));

      expect(beforeSend).toHaveBeenCalled();
      expect(transport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          tags: { modified: 'true' },
        })
      );
    });

    it('should drop event if beforeSend returns null', () => {
      const client = new ErrorTrackingClient({
        ...defaultOptions,
        beforeSend: () => null,
      });
      const transport = (client as any).transport;

      client.captureException(new Error('Test'));

      expect(transport.sendEvent).not.toHaveBeenCalled();
    });
  });

  describe('captureMessage', () => {
    it('should capture message with default level', () => {
      const eventId = client.captureMessage('Test message');

      expect(eventId).toBeTruthy();
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          message: 'Test message',
          level: 'info',
        })
      );
    });

    it('should capture message with custom level', () => {
      const eventId = client.captureMessage('Error message', 'error');

      expect(eventId).toBeTruthy();
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          message: 'Error message',
          level: 'error',
        })
      );
    });

    it('should apply capture context to message', () => {
      client.captureMessage('Test', 'warning', {
        tags: { source: 'test' },
      });

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          message: 'Test',
          level: 'warning',
          tags: { source: 'test' },
        })
      );
    });
  });

  describe('captureEvent', () => {
    it('should capture custom event', () => {
      const event: Event = {
        message: 'Custom event',
        level: 'info',
        tags: { custom: 'true' },
      };

      const eventId = client.captureEvent(event);

      expect(eventId).toBeTruthy();
      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          event_id: eventId,
          message: 'Custom event',
          level: 'info',
          tags: { custom: 'true' },
        })
      );
    });

    it('should enrich event with SDK info', () => {
      const event: Event = { message: 'Test' };

      client.captureEvent(event);

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          platform: 'node',
          sdk: {
            name: '@temps-sdk/node-sdk',
            version: '1.0.0',
          },
        })
      );
    });

    it('should preserve existing event_id', () => {
      const event: Event = {
        event_id: 'custom-id-123',
        message: 'Test',
      };

      const eventId = client.captureEvent(event);

      expect(eventId).toBe('custom-id-123');
    });
  });

  describe('context methods', () => {
    it('should set user context', () => {
      client.setUser({ id: '123', email: 'test@example.com' });
      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          user: { id: '123', email: 'test@example.com' },
        })
      );
    });

    it('should set tags', () => {
      client.setTag('key', 'value');
      client.setTags({ key2: 'value2' });
      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          tags: { key: 'value', key2: 'value2' },
        })
      );
    });

    it('should set extra data', () => {
      client.setExtra('data1', 'value1');
      client.setExtras({ data2: 'value2' });
      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          extra: { data1: 'value1', data2: 'value2' },
        })
      );
    });

    it('should set context', () => {
      client.setContext('device', { model: 'iPhone' });
      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          contexts: { device: { model: 'iPhone' } },
        })
      );
    });

    it('should add breadcrumbs', () => {
      client.addBreadcrumb({ message: 'Breadcrumb 1' });
      client.addBreadcrumb({ message: 'Breadcrumb 2' });
      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          breadcrumbs: expect.arrayContaining([
            expect.objectContaining({ message: 'Breadcrumb 1' }),
            expect.objectContaining({ message: 'Breadcrumb 2' }),
          ]),
        })
      );
    });

    it('should clear breadcrumbs', () => {
      client.addBreadcrumb({ message: 'Test' });
      client.clearBreadcrumbs();
      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          breadcrumbs: [],
        })
      );
    });
  });

  describe('scope management', () => {
    it('should configure scope', () => {
      client.configureScope((scope) => {
        scope.setTag('configured', 'true');
        scope.setUser({ id: 'scoped' });
      });

      client.captureMessage('Test');

      expect(mockTransport.sendEvent).toHaveBeenCalledWith(
        expect.objectContaining({
          tags: { configured: 'true' },
          user: { id: 'scoped' },
        })
      );
    });

    it('should work with temporary scope', () => {
      client.setTag('global', 'tag');

      client.withScope((scope) => {
        scope.setTag('temporary', 'tag');
        client.captureMessage('Scoped message');
      });

      client.captureMessage('Global message');

      expect(mockTransport.sendEvent).toHaveBeenNthCalledWith(1,
        expect.objectContaining({
          message: 'Scoped message',
          tags: { global: 'tag', temporary: 'tag' },
        })
      );

      expect(mockTransport.sendEvent).toHaveBeenNthCalledWith(2,
        expect.objectContaining({
          message: 'Global message',
          tags: { global: 'tag' },
        })
      );
    });
  });

  describe('flush and close', () => {
    it('should flush events', async () => {
      const result = await client.flush(100);
      expect(result).toBe(true);
    });

    it('should close and disable client', async () => {
      const result = await client.close(100);

      expect(result).toBe(true);
      expect((client as any).enabled).toBe(false);

      // Should not capture after closing
      client.captureMessage('After close');
      expect(mockTransport.sendEvent).not.toHaveBeenCalled();
    });
  });

  describe('error handling', () => {
    it('should handle transport errors gracefully', async () => {
      const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

      mockTransport.sendEvent.mockRejectedValueOnce(new Error('Transport error'));

      const client = new ErrorTrackingClient({
        ...defaultOptions,
        debug: true,
      });

      const transport = (client as any).transport;
      transport.sendEvent = mockTransport.sendEvent;

      client.captureMessage('Test');

      await new Promise(resolve => setTimeout(resolve, 0));

      expect(consoleSpy).toHaveBeenCalledWith(
        'Failed to send event:',
        expect.any(Error)
      );

      consoleSpy.mockRestore();
    });
  });
});
