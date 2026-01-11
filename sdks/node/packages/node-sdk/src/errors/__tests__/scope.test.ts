import { describe, it, expect, beforeEach } from 'vitest';
import { Scope } from '../scope';
import type { Event, Breadcrumb, User } from '../types';

describe('Scope', () => {
  let scope: Scope;

  beforeEach(() => {
    scope = new Scope(100);
  });

  describe('user management', () => {
    it('should set and apply user to event', () => {
      const user: User = {
        id: '123',
        email: 'test@example.com',
        username: 'testuser',
      };

      scope.setUser(user);

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.user).toEqual(user);
    });

    it('should clear user when set to null', () => {
      scope.setUser({ id: '123' });
      scope.setUser(null);

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.user).toBeUndefined();
    });

    it('should merge user data with event user', () => {
      scope.setUser({ id: '123', email: 'scope@example.com' });

      const event: Event = {
        user: { username: 'eventuser', email: 'event@example.com' },
      };

      const result = scope.applyToEvent(event);

      expect(result.user).toEqual({
        id: '123',
        email: 'event@example.com',
        username: 'eventuser',
      });
    });
  });

  describe('tags management', () => {
    it('should set individual tag', () => {
      scope.setTag('environment', 'production');

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.tags).toEqual({ environment: 'production' });
    });

    it('should set multiple tags', () => {
      scope.setTags({
        environment: 'production',
        version: '1.0.0',
      });

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.tags).toEqual({
        environment: 'production',
        version: '1.0.0',
      });
    });

    it('should merge tags with event tags', () => {
      scope.setTag('scopeTag', 'scopeValue');

      const event: Event = {
        tags: { eventTag: 'eventValue' },
      };

      const result = scope.applyToEvent(event);

      expect(result.tags).toEqual({
        scopeTag: 'scopeValue',
        eventTag: 'eventValue',
      });
    });
  });

  describe('extra data management', () => {
    it('should set individual extra data', () => {
      scope.setExtra('requestId', '12345');

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.extra).toEqual({ requestId: '12345' });
    });

    it('should set multiple extras', () => {
      scope.setExtras({
        requestId: '12345',
        sessionId: 'abc',
      });

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.extra).toEqual({
        requestId: '12345',
        sessionId: 'abc',
      });
    });

    it('should handle any type of extra data', () => {
      scope.setExtra('array', [1, 2, 3]);
      scope.setExtra('object', { nested: true });
      scope.setExtra('number', 42);

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.extra).toEqual({
        array: [1, 2, 3],
        object: { nested: true },
        number: 42,
      });
    });
  });

  describe('contexts management', () => {
    it('should set context', () => {
      scope.setContext('os', { name: 'Linux', version: '5.0' });

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.contexts).toEqual({
        os: { name: 'Linux', version: '5.0' },
      });
    });

    it('should remove context when set to null', () => {
      scope.setContext('os', { name: 'Linux' });
      scope.setContext('os', null);

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.contexts).toEqual({});
    });
  });

  describe('level management', () => {
    it('should set and apply level', () => {
      scope.setLevel('error');

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.level).toBe('error');
    });

    it('should not override event level', () => {
      scope.setLevel('warning');

      const event: Event = { level: 'error' };
      const result = scope.applyToEvent(event);

      expect(result.level).toBe('error');
    });
  });

  describe('breadcrumbs management', () => {
    it('should add breadcrumb with timestamp', () => {
      const breadcrumb: Breadcrumb = {
        message: 'User clicked button',
        category: 'ui.click',
      };

      scope.addBreadcrumb(breadcrumb);

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.breadcrumbs).toHaveLength(1);
      expect(result.breadcrumbs![0]).toMatchObject({
        message: 'User clicked button',
        category: 'ui.click',
      });
      expect(result.breadcrumbs![0].timestamp).toBeGreaterThan(0);
    });

    it('should respect maxBreadcrumbs limit', () => {
      const scope = new Scope(3);

      for (let i = 0; i < 5; i++) {
        scope.addBreadcrumb({
          message: `Breadcrumb ${i}`,
        });
      }

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.breadcrumbs).toHaveLength(3);
      expect(result.breadcrumbs![0].message).toBe('Breadcrumb 2');
      expect(result.breadcrumbs![2].message).toBe('Breadcrumb 4');
    });

    it('should clear all breadcrumbs', () => {
      scope.addBreadcrumb({ message: 'Breadcrumb 1' });
      scope.addBreadcrumb({ message: 'Breadcrumb 2' });
      scope.clearBreadcrumbs();

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.breadcrumbs).toEqual([]);
    });

    it('should merge with event breadcrumbs', () => {
      scope.addBreadcrumb({ message: 'Scope breadcrumb' });

      const event: Event = {
        breadcrumbs: [{ message: 'Event breadcrumb', timestamp: Date.now() }],
      };

      const result = scope.applyToEvent(event);

      expect(result.breadcrumbs).toHaveLength(2);
      expect(result.breadcrumbs![0].message).toBe('Scope breadcrumb');
      expect(result.breadcrumbs![1].message).toBe('Event breadcrumb');
    });
  });

  describe('clear', () => {
    it('should clear all scope data', () => {
      scope.setUser({ id: '123' });
      scope.setTag('tag', 'value');
      scope.setExtra('extra', 'data');
      scope.setContext('ctx', { key: 'value' });
      scope.setLevel('error');
      scope.addBreadcrumb({ message: 'breadcrumb' });

      scope.clear();

      const event: Event = {};
      const result = scope.applyToEvent(event);

      expect(result.user).toBeUndefined();
      expect(result.tags).toEqual({});
      expect(result.extra).toEqual({});
      expect(result.contexts).toEqual({});
      expect(result.level).toBeUndefined();
      expect(result.breadcrumbs).toEqual([]);
    });
  });

  describe('clone', () => {
    it('should create independent copy of scope', () => {
      scope.setUser({ id: '123' });
      scope.setTag('original', 'value');
      scope.addBreadcrumb({ message: 'original' });

      const cloned = scope.clone();

      cloned.setUser({ id: '456' });
      cloned.setTag('cloned', 'value');
      cloned.addBreadcrumb({ message: 'cloned' });

      const originalEvent: Event = {};
      const originalResult = scope.applyToEvent(originalEvent);

      const clonedEvent: Event = {};
      const clonedResult = cloned.applyToEvent(clonedEvent);

      expect(originalResult.user).toEqual({ id: '123' });
      expect(originalResult.tags).toEqual({ original: 'value' });
      expect(originalResult.breadcrumbs).toHaveLength(1);
      expect(originalResult.breadcrumbs![0].message).toBe('original');

      expect(clonedResult.user).toEqual({ id: '456' });
      expect(clonedResult.tags).toEqual({ original: 'value', cloned: 'value' });
      expect(clonedResult.breadcrumbs).toHaveLength(2);
    });

    it('should preserve maxBreadcrumbs in clone', () => {
      const scope = new Scope(5);
      const cloned = scope.clone();

      for (let i = 0; i < 10; i++) {
        cloned.addBreadcrumb({ message: `Breadcrumb ${i}` });
      }

      const event: Event = {};
      const result = cloned.applyToEvent(event);

      expect(result.breadcrumbs).toHaveLength(5);
    });
  });
});
