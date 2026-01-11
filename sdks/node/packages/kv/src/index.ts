import { KV, createClient } from './client.js';
import type { KVClientConfig, SetOptions } from './types.js';

// Re-export everything
export { KV, createClient } from './client.js';
export { KVError } from './errors.js';
export type {
  KVClientConfig,
  SetOptions,
  GetResponse,
  SetResponse,
  DelResponse,
  IncrResponse,
  ExpireResponse,
  TtlResponse,
  KeysResponse,
} from './types.js';

// Singleton instance for convenience functions
let defaultClient: KV | null = null;

function getDefaultClient(): KV {
  if (!defaultClient) {
    defaultClient = createClient();
  }
  return defaultClient;
}

/**
 * Default KV instance using environment variables
 * Use this for quick access: `import { kv } from '@temps-sdk/kv'`
 */
export const kv = {
  get: <T = unknown>(key: string): Promise<T | null> => getDefaultClient().get<T>(key),
  set: (key: string, value: unknown, options?: SetOptions): Promise<'OK' | null> =>
    getDefaultClient().set(key, value, options),
  del: (...keys: string[]): Promise<number> => getDefaultClient().del(...keys),
  incr: (key: string): Promise<number> => getDefaultClient().incr(key),
  expire: (key: string, seconds: number): Promise<number> => getDefaultClient().expire(key, seconds),
  ttl: (key: string): Promise<number> => getDefaultClient().ttl(key),
  keys: (pattern: string): Promise<string[]> => getDefaultClient().keys(pattern),
};

// Convenience functions that use the default client
/**
 * Get the value of a key
 */
export async function get<T = unknown>(key: string): Promise<T | null> {
  return getDefaultClient().get<T>(key);
}

/**
 * Set the value of a key
 */
export async function set(key: string, value: unknown, options?: SetOptions): Promise<'OK' | null> {
  return getDefaultClient().set(key, value, options);
}

/**
 * Delete one or more keys
 */
export async function del(...keys: string[]): Promise<number> {
  return getDefaultClient().del(...keys);
}

/**
 * Increment a numeric value
 */
export async function incr(key: string): Promise<number> {
  return getDefaultClient().incr(key);
}

/**
 * Set expiration on a key
 */
export async function expire(key: string, seconds: number): Promise<number> {
  return getDefaultClient().expire(key, seconds);
}

/**
 * Get the time to live of a key
 */
export async function ttl(key: string): Promise<number> {
  return getDefaultClient().ttl(key);
}

/**
 * Find keys matching a pattern
 */
export async function keys(pattern: string): Promise<string[]> {
  return getDefaultClient().keys(pattern);
}

/**
 * Reset the default client (useful for testing)
 * @internal
 */
export function _resetDefaultClient(): void {
  defaultClient = null;
}
