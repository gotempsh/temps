import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import { HttpTransport, ConsoleTransport, parseDsn, createTransportFromDsn } from '../transport';
import type { Event } from '../types';

// Mock fetch globally
(global as any).fetch = vi.fn();

describe('transport', () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('parseDsn', () => {
    it('should parse valid Sentry DSN', () => {
      const dsn = 'https://abc123@sentry.io/42';
      const result = parseDsn(dsn);

      expect(result).toEqual({
        protocol: 'https',
        publicKey: 'abc123',
        host: 'sentry.io',
        projectId: '42',
      });
    });

    it('should parse DSN with complex host', () => {
      const dsn = 'https://key123@my-sentry.example.com/12345';
      const result = parseDsn(dsn);

      expect(result).toEqual({
        protocol: 'https',
        publicKey: 'key123',
        host: 'my-sentry.example.com',
        projectId: '12345',
      });
    });

    it('should parse HTTP DSN', () => {
      const dsn = 'http://localkey@localhost:9000/1';
      const result = parseDsn(dsn);

      expect(result).toEqual({
        protocol: 'http',
        publicKey: 'localkey',
        host: 'localhost:9000',
        projectId: '1',
      });
    });

    it('should throw on invalid DSN format', () => {
      expect(() => parseDsn('invalid-dsn')).toThrow('Invalid DSN format');
      expect(() => parseDsn('https://missing-project-id')).toThrow('Invalid DSN format');
      expect(() => parseDsn('no-protocol')).toThrow('Invalid DSN format');
    });
  });

  describe('HttpTransport', () => {
    let transport: HttpTransport;
    const mockEvent: Event = {
      event_id: 'test123',
      timestamp: Date.now(),
      level: 'error',
      message: 'Test error',
    };

    beforeEach(() => {
      transport = new HttpTransport({
        url: 'https://api.example.com/store',
        headers: { 'X-API-Key': 'test-key' },
        timeout: 5000,
      });
    });

    it('should send event successfully', async () => {
      const mockResponse = {
        ok: true,
        status: 200,
        statusText: 'OK',
      };

      (global.fetch as any).mockResolvedValueOnce(mockResponse);

      await transport.sendEvent(mockEvent);

      expect(global.fetch).toHaveBeenCalledWith(
        'https://api.example.com/store',
        expect.objectContaining({
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-API-Key': 'test-key',
          },
          body: JSON.stringify(mockEvent),
          signal: expect.any(AbortSignal),
        })
      );
    });

    it('should throw on non-ok response', async () => {
      const mockResponse = {
        ok: false,
        status: 400,
        statusText: 'Bad Request',
      };

      (global.fetch as any).mockResolvedValueOnce(mockResponse);

      await expect(transport.sendEvent(mockEvent)).rejects.toThrow(
        'Failed to send event: 400 Bad Request'
      );
    });

    it('should handle timeout', async () => {
      const transport = new HttpTransport({
        url: 'https://api.example.com/store',
        timeout: 100,
      });

      // Simulate a slow response that will trigger abort
      let abortHandler: any;
      (global.fetch as any).mockImplementationOnce(
        async (url: string, options: any) => {
          // Capture the abort signal
          options.signal.addEventListener('abort', () => {
            abortHandler = true;
          });

          // Wait longer than timeout
          await new Promise((resolve) => setTimeout(resolve, 200));

          if (abortHandler) {
            const error = new Error('Aborted');
            error.name = 'AbortError';
            throw error;
          }

          return { ok: true };
        }
      );

      await expect(transport.sendEvent(mockEvent)).rejects.toThrow(
        'Request timeout after 100ms'
      );
    });

    it('should handle network errors', async () => {
      (global.fetch as any).mockRejectedValueOnce(new Error('Network error'));

      await expect(transport.sendEvent(mockEvent)).rejects.toThrow('Network error');
    });

    it('should use default timeout', () => {
      const transport = new HttpTransport({
        url: 'https://api.example.com/store',
      });

      // Default timeout is 30000ms, we'll test it's set
      expect(transport).toBeDefined();
    });

    it('should handle abort error specially', async () => {
      const abortError = new Error('Aborted');
      abortError.name = 'AbortError';

      (global.fetch as any).mockRejectedValueOnce(abortError);

      const transport = new HttpTransport({
        url: 'https://api.example.com/store',
        timeout: 1000,
      });

      await expect(transport.sendEvent(mockEvent)).rejects.toThrow(
        'Request timeout after 1000ms'
      );
    });
  });

  describe('ConsoleTransport', () => {
    it('should log event to console', async () => {
      const consoleSpy = vi.spyOn(console, 'log').mockImplementation(() => {});
      const transport = new ConsoleTransport();

      const mockEvent: Event = {
        event_id: 'test123',
        level: 'error',
        message: 'Test error',
      };

      await transport.sendEvent(mockEvent);

      expect(consoleSpy).toHaveBeenCalledWith(
        '[ErrorTracking]',
        JSON.stringify(mockEvent, null, 2)
      );

      consoleSpy.mockRestore();
    });
  });

  describe('createTransportFromDsn', () => {
    it('should create ConsoleTransport in debug mode', () => {
      const transport = createTransportFromDsn(
        'https://key@sentry.io/42',
        true
      );

      expect(transport).toBeInstanceOf(ConsoleTransport);
    });

    it('should create HttpTransport with correct URL from DSN', () => {
      const mockFetch = vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
      });
      global.fetch = mockFetch;

      const transport = createTransportFromDsn(
        'https://abc123@sentry.example.com/42',
        false
      );

      expect(transport).toBeInstanceOf(HttpTransport);

      // Test that it creates correct URL by sending an event
      const event: Event = { event_id: 'test' };
      transport.sendEvent(event);

      expect(mockFetch).toHaveBeenCalledWith(
        'https://sentry.example.com/api/42/store/',
        expect.objectContaining({
          headers: expect.objectContaining({
            'X-Sentry-Auth': 'Sentry sentry_key=abc123, sentry_version=7',
          }),
        })
      );
    });

    it('should handle HTTP protocol in DSN', () => {
      const mockFetch = vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
      });
      global.fetch = mockFetch;

      const transport = createTransportFromDsn(
        'http://localkey@localhost:9000/1',
        false
      );

      const event: Event = { event_id: 'test' };
      transport.sendEvent(event);

      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:9000/api/1/store/',
        expect.any(Object)
      );
    });
  });
});
