import { describe, it, expect, beforeEach } from 'vitest';
import {
  mockFetch,
  createMockResponse,
  createErrorResponse,
  TEST_API_URL,
  TEST_TOKEN,
} from './setup.js';
import { KV, createClient, kv, get, set, del, incr, expire, ttl, keys, _resetDefaultClient, KVError } from '../index.js';

describe('KV Client', () => {
  describe('constructor', () => {
    it('should create client with environment variables', () => {
      const client = new KV();
      expect(client).toBeInstanceOf(KV);
    });

    it('should create client with explicit config', () => {
      const client = new KV({
        apiUrl: 'https://custom.api.test',
        token: 'custom-token',
      });
      expect(client).toBeInstanceOf(KV);
    });

    it('should throw when apiUrl is missing', () => {
      delete process.env.TEMPS_API_URL;
      expect(() => new KV()).toThrow(KVError);
      expect(() => new KV()).toThrow('Missing required configuration: apiUrl');
    });

    it('should throw when token is missing', () => {
      delete process.env.TEMPS_TOKEN;
      expect(() => new KV()).toThrow(KVError);
      expect(() => new KV()).toThrow('Missing required configuration: token');
    });

    it('should remove trailing slash from apiUrl', () => {
      const client = new KV({
        apiUrl: 'https://api.test/',
        token: 'token',
      });
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 'test' }));
      client.get('key');
      expect(mockFetch).toHaveBeenCalledWith(
        'https://api.test/api/kv/get',
        expect.any(Object)
      );
    });
  });

  describe('createClient', () => {
    it('should create a new KV instance', () => {
      const client = createClient();
      expect(client).toBeInstanceOf(KV);
    });

    it('should accept config options', () => {
      const client = createClient({
        apiUrl: 'https://custom.api.test',
        token: 'custom-token',
      });
      expect(client).toBeInstanceOf(KV);
    });
  });

  describe('get', () => {
    it('should get a value by key', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({ value: { name: 'John', age: 30 } })
      );

      const result = await client.get<{ name: string; age: number }>('user:123');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/get`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ key: 'user:123' }),
        }
      );
      expect(result).toEqual({ name: 'John', age: 30 });
    });

    it('should return null for non-existent key', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: null }));

      const result = await client.get('nonexistent');
      expect(result).toBeNull();
    });

    it('should throw KVError on API error', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(
        createErrorResponse('Key not found', 'KEY_NOT_FOUND', 404)
      );

      try {
        await client.get('missing');
        expect.fail('Expected KVError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(KVError);
        expect(error).toMatchObject({
          message: 'Key not found',
          code: 'KEY_NOT_FOUND',
          status: 404,
        });
      }
    });
  });

  describe('set', () => {
    it('should set a value', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));

      const result = await client.set('user:123', { name: 'John' });

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/set`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ key: 'user:123', value: { name: 'John' } }),
        }
      );
      expect(result).toBe('OK');
    });

    it('should set a value with expiration in seconds (ex)', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));

      await client.set('key', 'value', { ex: 3600 });

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ key: 'key', value: 'value', ex: 3600 }),
        })
      );
    });

    it('should set a value with expiration in milliseconds (px)', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));

      await client.set('key', 'value', { px: 60000 });

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ key: 'key', value: 'value', px: 60000 }),
        })
      );
    });

    it('should set only if key does not exist (nx)', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));

      await client.set('key', 'value', { nx: true });

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ key: 'key', value: 'value', nx: true }),
        })
      );
    });

    it('should set only if key exists (xx)', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));

      await client.set('key', 'value', { xx: true });

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ key: 'key', value: 'value', xx: true }),
        })
      );
    });

    it('should return null when conditional set fails', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: null }));

      const result = await client.set('key', 'value', { nx: true });
      expect(result).toBeNull();
    });

    it('should combine multiple options', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));

      await client.set('key', 'value', { ex: 3600, nx: true });

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ key: 'key', value: 'value', ex: 3600, nx: true }),
        })
      );
    });
  });

  describe('del', () => {
    it('should delete a single key', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ deleted: 1 }));

      const result = await client.del('user:123');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/del`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ keys: ['user:123'] }),
        }
      );
      expect(result).toBe(1);
    });

    it('should delete multiple keys', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ deleted: 3 }));

      const result = await client.del('key1', 'key2', 'key3');

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ keys: ['key1', 'key2', 'key3'] }),
        })
      );
      expect(result).toBe(3);
    });

    it('should return 0 when no keys deleted', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ deleted: 0 }));

      const result = await client.del('nonexistent');
      expect(result).toBe(0);
    });
  });

  describe('incr', () => {
    it('should increment a value', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 1 }));

      const result = await client.incr('counter');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/incr`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ key: 'counter' }),
        }
      );
      expect(result).toBe(1);
    });

    it('should return incremented value', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 42 }));

      const result = await client.incr('counter');
      expect(result).toBe(42);
    });
  });

  describe('expire', () => {
    it('should set expiration on a key', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 1 }));

      const result = await client.expire('key', 3600);

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/expire`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ key: 'key', seconds: 3600 }),
        }
      );
      expect(result).toBe(1);
    });

    it('should return 0 if key does not exist', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 0 }));

      const result = await client.expire('nonexistent', 3600);
      expect(result).toBe(0);
    });
  });

  describe('ttl', () => {
    it('should get time to live of a key', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ ttl: 3600 }));

      const result = await client.ttl('key');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/ttl`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ key: 'key' }),
        }
      );
      expect(result).toBe(3600);
    });

    it('should return -2 for non-existent key', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ ttl: -2 }));

      const result = await client.ttl('nonexistent');
      expect(result).toBe(-2);
    });

    it('should return -1 for key with no expiry', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ ttl: -1 }));

      const result = await client.ttl('permanent');
      expect(result).toBe(-1);
    });
  });

  describe('keys', () => {
    it('should find keys matching a pattern', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({ keys: ['user:1', 'user:2', 'user:3'] })
      );

      const result = await client.keys('user:*');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/kv/keys`,
        {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          },
          body: JSON.stringify({ pattern: 'user:*' }),
        }
      );
      expect(result).toEqual(['user:1', 'user:2', 'user:3']);
    });

    it('should return empty array when no keys match', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(createMockResponse({ keys: [] }));

      const result = await client.keys('nonexistent:*');
      expect(result).toEqual([]);
    });
  });

  describe('error handling', () => {
    it('should handle network errors', async () => {
      const client = new KV();
      mockFetch.mockRejectedValueOnce(new Error('Network failure'));

      try {
        await client.get('key');
        expect.fail('Expected KVError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(KVError);
        expect((error as KVError).code).toBe('NETWORK_ERROR');
        expect((error as KVError).message).toContain('Network failure');
      }
    });

    it('should handle server errors', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(
        createErrorResponse('Internal server error', 'INTERNAL_ERROR', 500)
      );

      try {
        await client.get('key');
        expect.fail('Expected KVError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(KVError);
        expect((error as KVError).status).toBe(500);
        expect((error as KVError).code).toBe('INTERNAL_ERROR');
      }
    });

    it('should handle unauthorized errors', async () => {
      const client = new KV();
      mockFetch.mockResolvedValueOnce(
        createErrorResponse('Invalid token', 'UNAUTHORIZED', 401)
      );

      try {
        await client.get('key');
        expect.fail('Expected KVError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(KVError);
        expect((error as KVError).status).toBe(401);
        expect((error as KVError).code).toBe('UNAUTHORIZED');
      }
    });
  });
});

describe('Convenience Functions', () => {
  beforeEach(() => {
    _resetDefaultClient();
  });

  describe('kv object', () => {
    it('should get a value using kv.get', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 'test' }));
      const result = await kv.get('key');
      expect(result).toBe('test');
    });

    it('should set a value using kv.set', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));
      const result = await kv.set('key', 'value');
      expect(result).toBe('OK');
    });

    it('should delete using kv.del', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ deleted: 1 }));
      const result = await kv.del('key');
      expect(result).toBe(1);
    });

    it('should increment using kv.incr', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 1 }));
      const result = await kv.incr('counter');
      expect(result).toBe(1);
    });

    it('should set expiration using kv.expire', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 1 }));
      const result = await kv.expire('key', 3600);
      expect(result).toBe(1);
    });

    it('should get ttl using kv.ttl', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ ttl: 3600 }));
      const result = await kv.ttl('key');
      expect(result).toBe(3600);
    });

    it('should find keys using kv.keys', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ keys: ['key1', 'key2'] }));
      const result = await kv.keys('key*');
      expect(result).toEqual(['key1', 'key2']);
    });
  });

  describe('standalone functions', () => {
    it('should get a value using get()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 'test' }));
      const result = await get('key');
      expect(result).toBe('test');
    });

    it('should set a value using set()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 'OK' }));
      const result = await set('key', 'value', { ex: 3600 });
      expect(result).toBe('OK');
    });

    it('should delete using del()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ deleted: 2 }));
      const result = await del('key1', 'key2');
      expect(result).toBe(2);
    });

    it('should increment using incr()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ value: 5 }));
      const result = await incr('counter');
      expect(result).toBe(5);
    });

    it('should set expiration using expire()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ result: 1 }));
      const result = await expire('key', 7200);
      expect(result).toBe(1);
    });

    it('should get ttl using ttl()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ ttl: -1 }));
      const result = await ttl('key');
      expect(result).toBe(-1);
    });

    it('should find keys using keys()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({ keys: [] }));
      const result = await keys('nonexistent:*');
      expect(result).toEqual([]);
    });
  });

  describe('singleton behavior', () => {
    it('should reuse the same client instance', async () => {
      mockFetch.mockResolvedValue(createMockResponse({ value: 'test' }));

      await kv.get('key1');
      await kv.get('key2');

      // Both calls should use the same client (same auth header)
      expect(mockFetch).toHaveBeenCalledTimes(2);
      const [, firstCall] = mockFetch.mock.calls[0];
      const [, secondCall] = mockFetch.mock.calls[1];
      expect(firstCall.headers.Authorization).toBe(secondCall.headers.Authorization);
    });

    it('should reset default client', async () => {
      mockFetch.mockResolvedValue(createMockResponse({ value: 'test' }));

      await kv.get('key');
      _resetDefaultClient();

      // Change env vars
      process.env.TEMPS_TOKEN = 'new-token';

      await kv.get('key');

      // Second call should use new token
      const [, secondCall] = mockFetch.mock.calls[1];
      expect(secondCall.headers.Authorization).toBe('Bearer new-token');
    });
  });
});

describe('KVError', () => {
  it('should create error with message only', () => {
    const error = new KVError('Something went wrong');
    expect(error.message).toBe('Something went wrong');
    expect(error.name).toBe('KVError');
    expect(error.code).toBeUndefined();
    expect(error.status).toBeUndefined();
  });

  it('should create error with code', () => {
    const error = new KVError('Not found', 'NOT_FOUND');
    expect(error.message).toBe('Not found');
    expect(error.code).toBe('NOT_FOUND');
  });

  it('should create error with status', () => {
    const error = new KVError('Unauthorized', 'UNAUTHORIZED', 401);
    expect(error.message).toBe('Unauthorized');
    expect(error.code).toBe('UNAUTHORIZED');
    expect(error.status).toBe(401);
  });

  it('should create from response', () => {
    const response = {
      ok: false,
      status: 404,
    } as Response;
    const body = { error: { message: 'Key not found', code: 'KEY_NOT_FOUND' } };

    const error = KVError.fromResponse(response, body);
    expect(error.message).toBe('Key not found');
    expect(error.code).toBe('KEY_NOT_FOUND');
    expect(error.status).toBe(404);
  });

  it('should create from response without body', () => {
    const response = {
      ok: false,
      status: 500,
    } as Response;

    const error = KVError.fromResponse(response);
    expect(error.message).toBe('KV operation failed with status 500');
    expect(error.status).toBe(500);
  });

  it('should create missing config error for apiUrl', () => {
    const error = KVError.missingConfig('apiUrl');
    expect(error.message).toContain('TEMPS_API_URL');
    expect(error.code).toBe('MISSING_CONFIG');
  });

  it('should create missing config error for token', () => {
    const error = KVError.missingConfig('token');
    expect(error.message).toContain('TEMPS_TOKEN');
    expect(error.code).toBe('MISSING_CONFIG');
  });

  it('should create network error', () => {
    const originalError = new Error('Connection refused');
    const error = KVError.networkError(originalError);
    expect(error.message).toContain('Connection refused');
    expect(error.code).toBe('NETWORK_ERROR');
  });

  it('should be instanceof Error', () => {
    const error = new KVError('Test');
    expect(error).toBeInstanceOf(Error);
    expect(error).toBeInstanceOf(KVError);
  });
});
