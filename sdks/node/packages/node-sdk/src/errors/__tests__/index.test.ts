import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import * as ErrorTracking from '../index';

describe('ErrorTracking public API', () => {
  let consoleSpy: any;

  beforeEach(() => {
    consoleSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    // Reset the global client using test helper
    ErrorTracking.__resetForTesting();
  });

  afterEach(() => {
    consoleSpy.mockRestore();
    vi.clearAllMocks();
  });

  describe('init', () => {
    it('should initialize global client', () => {
      expect(ErrorTracking.getCurrentClient()).toBe(null);

      ErrorTracking.init({
        dsn: 'https://test@sentry.io/42',
        environment: 'test',
      });

      const client = ErrorTracking.getCurrentClient();
      expect(client).toBeDefined();
      expect(client).not.toBe(null);
    });
  });

  describe('without initialization', () => {
    it('should warn and return empty string when captureException is called without init', () => {
      const result = ErrorTracking.captureException(new Error('Test'));

      expect(result).toBe('');
      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn and return empty string when captureMessage is called without init', () => {
      const result = ErrorTracking.captureMessage('Test message');

      expect(result).toBe('');
      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn and return empty string when captureEvent is called without init', () => {
      const result = ErrorTracking.captureEvent({ message: 'Test' });

      expect(result).toBe('');
      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when setUser is called without init', () => {
      ErrorTracking.setUser({ id: '123' });

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when setTag is called without init', () => {
      ErrorTracking.setTag('key', 'value');

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when setTags is called without init', () => {
      ErrorTracking.setTags({ key: 'value' });

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when setExtra is called without init', () => {
      ErrorTracking.setExtra('key', 'value');

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when setExtras is called without init', () => {
      ErrorTracking.setExtras({ key: 'value' });

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when setContext is called without init', () => {
      ErrorTracking.setContext('device', { model: 'iPhone' });

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when addBreadcrumb is called without init', () => {
      ErrorTracking.addBreadcrumb({ message: 'Test' });

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when clearBreadcrumbs is called without init', () => {
      ErrorTracking.clearBreadcrumbs();

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when configureScope is called without init', () => {
      ErrorTracking.configureScope(() => {});

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should warn when withScope is called without init', () => {
      ErrorTracking.withScope(() => {});

      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should return false when flush is called without init', async () => {
      const result = await ErrorTracking.flush();

      expect(result).toBe(false);
      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });

    it('should return false when close is called without init', async () => {
      const result = await ErrorTracking.close();

      expect(result).toBe(false);
      expect(consoleSpy).toHaveBeenCalledWith(
        'Error tracking client not initialized. Call init() first.'
      );
    });
  });

  describe('with initialization', () => {
    beforeEach(() => {
      // Mock the transport to prevent actual network calls
      vi.mock('../transport', () => ({
        createTransportFromDsn: vi.fn(() => ({
          sendEvent: vi.fn().mockResolvedValue(undefined),
        })),
      }));

      ErrorTracking.init({
        dsn: 'https://test@sentry.io/42',
        debug: true, // Use console transport
      });
    });

    it('should capture exceptions after init', () => {
      const error = new Error('Test error');
      const result = ErrorTracking.captureException(error);

      expect(result).toBeTruthy();
      expect(result).toMatch(/^[a-f0-9]{32}$/); // Event ID format
    });

    it('should capture messages after init', () => {
      const result = ErrorTracking.captureMessage('Test message', 'error');

      expect(result).toBeTruthy();
      expect(result).toMatch(/^[a-f0-9]{32}$/);
    });

    it('should capture events after init', () => {
      const event = { message: 'Test event', level: 'info' as const };
      const result = ErrorTracking.captureEvent(event);

      expect(result).toBeTruthy();
      expect(result).toMatch(/^[a-f0-9]{32}$/);
    });

    it('should set user context after init', () => {
      const user = { id: '123', email: 'test@example.com' };
      ErrorTracking.setUser(user);

      // Capture an event to verify user was set
      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should set tags after init', () => {
      ErrorTracking.setTag('environment', 'production');
      ErrorTracking.setTags({ version: '1.0.0' });

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should set extra data after init', () => {
      ErrorTracking.setExtra('requestId', '12345');
      ErrorTracking.setExtras({ sessionId: 'abc' });

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should set context after init', () => {
      ErrorTracking.setContext('os', { name: 'Linux', version: '5.0' });

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should add breadcrumbs after init', () => {
      ErrorTracking.addBreadcrumb({ message: 'Breadcrumb 1' });
      ErrorTracking.addBreadcrumb({ message: 'Breadcrumb 2' });

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should clear breadcrumbs after init', () => {
      ErrorTracking.addBreadcrumb({ message: 'Test' });
      ErrorTracking.clearBreadcrumbs();

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should configure scope after init', () => {
      ErrorTracking.configureScope((scope) => {
        scope.setTag('configured', 'true');
      });

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should work with temporary scope after init', () => {
      ErrorTracking.withScope((scope) => {
        scope.setTag('temporary', 'true');
      });

      const result = ErrorTracking.captureMessage('Test');
      expect(result).toBeTruthy();
    });

    it('should flush events after init', async () => {
      const result = await ErrorTracking.flush(100);
      expect(result).toBe(true);
    });

    it('should close client after init', async () => {
      const result = await ErrorTracking.close(100);
      expect(result).toBe(true);

      // After closing, client should be disabled
      const captureResult = ErrorTracking.captureMessage('After close');
      expect(captureResult).toBe('');
    });
  });

  describe('exports', () => {
    it('should export ErrorTrackingClient', () => {
      expect(ErrorTracking.ErrorTrackingClient).toBeDefined();
    });

    it('should export Scope', () => {
      expect(ErrorTracking.Scope).toBeDefined();
    });

    it('should export getCurrentClient', () => {
      expect(ErrorTracking.getCurrentClient).toBeDefined();
    });
  });
});
