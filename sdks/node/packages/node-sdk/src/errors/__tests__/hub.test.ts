import { describe, it, expect, beforeEach, vi } from 'vitest';
import { Hub } from '../hub';
import type { Event, CaptureContext } from '../types';

describe('Hub', () => {
  let hub: Hub;

  beforeEach(() => {
    hub = new Hub(100);
  });

  describe('scope management', () => {
    it('should start with one global scope', () => {
      const scope = hub.getScope();
      expect(scope).toBeDefined();
    });

    it('should push and pop scopes', () => {
      const originalScope = hub.getScope();
      originalScope.setTag('original', 'true');

      const newScope = hub.pushScope();
      expect(newScope).not.toBe(originalScope);

      newScope.setTag('new', 'true');

      const event1: Event = {};
      const result1 = newScope.applyToEvent(event1);
      expect(result1.tags).toEqual({
        original: 'true',
        new: 'true',
      });

      hub.popScope();

      const currentScope = hub.getScope();
      expect(currentScope).toBe(originalScope);

      const event2: Event = {};
      const result2 = currentScope.applyToEvent(event2);
      expect(result2.tags).toEqual({ original: 'true' });
    });

    it('should not pop the global scope', () => {
      const originalScope = hub.getScope();

      hub.popScope();
      hub.popScope();
      hub.popScope();

      const currentScope = hub.getScope();
      expect(currentScope).toBe(originalScope);
    });

    it('should clone scope when pushing', () => {
      const originalScope = hub.getScope();
      originalScope.setUser({ id: '123' });

      const newScope = hub.pushScope();
      // The new scope starts with cloned data from original
      const event1: Event = {};
      const result1 = newScope.applyToEvent(event1);
      expect(result1.user).toEqual({ id: '123' }); // Has parent's user

      // Now modify the new scope
      newScope.setUser({ id: '456' });
      const event2: Event = {};
      const result2 = newScope.applyToEvent(event2);
      expect(result2.user).toEqual({ id: '456' }); // Has new user

      // Original scope is unaffected
      const event3: Event = {};
      const result3 = originalScope.applyToEvent(event3);
      expect(result3.user).toEqual({ id: '123' }); // Still has original user
    });
  });

  describe('withScope', () => {
    it('should execute callback with temporary scope', () => {
      hub.setTag('global', 'value');

      let capturedTags: Record<string, string> | undefined;

      hub.withScope((scope) => {
        scope.setTag('temporary', 'value');
        const event: Event = {};
        const result = hub.getScope().applyToEvent(event);
        capturedTags = result.tags;
      });

      expect(capturedTags).toEqual({
        global: 'value',
        temporary: 'value',
      });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);
      expect(result.tags).toEqual({ global: 'value' });
    });

    it('should pop scope even if callback throws', () => {
      const originalScope = hub.getScope();

      expect(() => {
        hub.withScope(() => {
          throw new Error('Test error');
        });
      }).toThrow('Test error');

      const currentScope = hub.getScope();
      expect(currentScope).toBe(originalScope);
    });
  });

  describe('configureScope', () => {
    it('should configure current scope', () => {
      hub.configureScope((scope) => {
        scope.setUser({ id: '123' });
        scope.setTag('configured', 'true');
      });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.user).toEqual({ id: '123' });
      expect(result.tags).toEqual({ configured: 'true' });
    });
  });

  describe('context methods', () => {
    it('should proxy setUser to current scope', () => {
      hub.setUser({ id: '123', email: 'test@example.com' });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.user).toEqual({
        id: '123',
        email: 'test@example.com',
      });
    });

    it('should proxy setTag to current scope', () => {
      hub.setTag('key', 'value');

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.tags).toEqual({ key: 'value' });
    });

    it('should proxy setTags to current scope', () => {
      hub.setTags({ key1: 'value1', key2: 'value2' });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.tags).toEqual({
        key1: 'value1',
        key2: 'value2',
      });
    });

    it('should proxy setExtra to current scope', () => {
      hub.setExtra('data', { nested: true });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.extra).toEqual({ data: { nested: true } });
    });

    it('should proxy setExtras to current scope', () => {
      hub.setExtras({ data1: 'value1', data2: 42 });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.extra).toEqual({
        data1: 'value1',
        data2: 42,
      });
    });

    it('should proxy setContext to current scope', () => {
      hub.setContext('device', { model: 'iPhone', os: 'iOS' });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.contexts).toEqual({
        device: { model: 'iPhone', os: 'iOS' },
      });
    });

    it('should proxy addBreadcrumb to current scope', () => {
      hub.addBreadcrumb({ message: 'Test breadcrumb' });

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.breadcrumbs).toHaveLength(1);
      expect(result.breadcrumbs![0].message).toBe('Test breadcrumb');
    });

    it('should proxy clearBreadcrumbs to current scope', () => {
      hub.addBreadcrumb({ message: 'Test 1' });
      hub.addBreadcrumb({ message: 'Test 2' });
      hub.clearBreadcrumbs();

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.breadcrumbs).toEqual([]);
    });
  });

  describe('applyToEvent', () => {
    it('should apply all scopes to event', () => {
      hub.setTag('global', 'value');
      hub.setUser({ id: 'global' });
      hub.setExtra('level', 0);

      const scope1 = hub.pushScope();
      scope1.setTag('scope1', 'value');
      scope1.setExtra('level', 1);

      const scope2 = hub.pushScope();
      scope2.setTag('scope2', 'value');
      scope2.setExtra('level', 2);

      const event: Event = {
        message: 'Test event',
      };

      const result = hub.applyToEvent(event);

      // All scopes are applied in order, with later scopes overriding earlier ones
      expect(result.tags).toEqual({
        global: 'value',
        scope1: 'value',
        scope2: 'value',
      });
      // Extra values are merged across all scopes
      expect(result.extra.level).toBe(2);
      // User is inherited through the scope chain
      expect(result.user).toEqual({ id: 'global' });
    });

    it('should apply captureContext as function', () => {
      hub.setTag('global', 'value');

      const captureContext: CaptureContext = (scope) => {
        scope.setTag('captured', 'value');
        scope.setUser({ id: 'captured' });
      };

      const event: Event = {};
      const result = hub.applyToEvent(event, captureContext);

      expect(result.tags).toEqual({
        global: 'value',
        captured: 'value',
      });
      expect(result.user).toEqual({ id: 'captured' });
    });

    it('should apply captureContext as partial scope', () => {
      hub.setTag('global', 'value');

      const captureContext: CaptureContext = {
        tags: { captured: 'value' },
        user: { id: 'captured' },
        extra: { data: 'captured' },
        contexts: { custom: { key: 'value' } },
        level: 'warning',
      };

      const event: Event = {};
      const result = hub.applyToEvent(event, captureContext);

      expect(result.tags).toEqual({
        global: 'value',
        captured: 'value',
      });
      expect(result.user).toEqual({ id: 'captured' });
      expect(result.extra).toEqual({ data: 'captured' });
      expect(result.contexts).toEqual({ custom: { key: 'value' } });
      expect(result.level).toBe('warning');
    });

    it('should not modify original scope when applying captureContext', () => {
      hub.setTag('global', 'value');

      const captureContext: CaptureContext = {
        tags: { captured: 'value' },
      };

      hub.applyToEvent({}, captureContext);

      const event: Event = {};
      const result = hub.getScope().applyToEvent(event);

      expect(result.tags).toEqual({ global: 'value' });
    });
  });
});
