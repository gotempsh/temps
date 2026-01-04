import type {
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
import { KVError } from './errors.js';

/**
 * KV client for interacting with the Temps KV store
 */
export class KV {
  private readonly apiUrl: string;
  private readonly token: string;
  private readonly projectId?: number;

  constructor(config: KVClientConfig = {}) {
    const apiUrl = config.apiUrl || process.env.TEMPS_API_URL;
    const token = config.token || process.env.TEMPS_TOKEN;
    const projectId = config.projectId ?? (process.env.TEMPS_PROJECT_ID ? parseInt(process.env.TEMPS_PROJECT_ID, 10) : undefined);

    if (!apiUrl) {
      throw KVError.missingConfig('apiUrl');
    }
    if (!token) {
      throw KVError.missingConfig('token');
    }

    // Remove trailing slash from API URL
    this.apiUrl = apiUrl.replace(/\/$/, '');
    this.token = token;
    this.projectId = projectId;
  }

  /**
   * Make an authenticated request to the KV API
   */
  private async request<T>(endpoint: string, body: Record<string, unknown>): Promise<T> {
    const url = `${this.apiUrl}/api/kv/${endpoint}`;

    // Include project_id in all requests when set
    const requestBody = this.projectId !== undefined
      ? { ...body, project_id: this.projectId }
      : body;

    try {
      const response = await fetch(url, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${this.token}`,
        },
        body: JSON.stringify(requestBody),
      });

      const data = await response.json() as T & { error?: { message: string; code?: string } };

      if (!response.ok) {
        throw KVError.fromResponse(response, data);
      }

      return data;
    } catch (error) {
      if (error instanceof KVError) {
        throw error;
      }
      throw KVError.networkError(error as Error);
    }
  }

  /**
   * Get the value of a key
   * @param key - The key to get
   * @returns The value or null if key doesn't exist
   */
  async get<T = unknown>(key: string): Promise<T | null> {
    const response = await this.request<GetResponse<T>>('get', { key });
    return response.value;
  }

  /**
   * Set the value of a key
   * @param key - The key to set
   * @param value - The value to set
   * @param options - Optional settings (expiration, conditional set)
   * @returns 'OK' on success, null if conditional set failed
   */
  async set(key: string, value: unknown, options?: SetOptions): Promise<'OK' | null> {
    const body: Record<string, unknown> = { key, value };

    if (options?.ex !== undefined) {
      body.ex = options.ex;
    }
    if (options?.px !== undefined) {
      body.px = options.px;
    }
    if (options?.nx !== undefined) {
      body.nx = options.nx;
    }
    if (options?.xx !== undefined) {
      body.xx = options.xx;
    }

    const response = await this.request<SetResponse>('set', body);
    return response.result;
  }

  /**
   * Delete one or more keys
   * @param keys - The keys to delete
   * @returns The number of keys deleted
   */
  async del(...keys: string[]): Promise<number> {
    const response = await this.request<DelResponse>('del', { keys });
    return response.deleted;
  }

  /**
   * Increment a numeric value
   * @param key - The key to increment
   * @returns The new value after incrementing
   */
  async incr(key: string): Promise<number> {
    const response = await this.request<IncrResponse>('incr', { key });
    return response.value;
  }

  /**
   * Set expiration on a key
   * @param key - The key to expire
   * @param seconds - Time to live in seconds
   * @returns 1 if timeout was set, 0 if key doesn't exist
   */
  async expire(key: string, seconds: number): Promise<number> {
    const response = await this.request<ExpireResponse>('expire', { key, seconds });
    return response.result;
  }

  /**
   * Get the time to live of a key
   * @param key - The key to check
   * @returns TTL in seconds, -2 if key doesn't exist, -1 if no expiry
   */
  async ttl(key: string): Promise<number> {
    const response = await this.request<TtlResponse>('ttl', { key });
    return response.ttl;
  }

  /**
   * Find keys matching a pattern
   * @param pattern - Pattern to match (e.g., 'user:*')
   * @returns Array of matching keys
   */
  async keys(pattern: string): Promise<string[]> {
    const response = await this.request<KeysResponse>('keys', { pattern });
    return response.keys;
  }
}

/**
 * Create a new KV client instance
 * @param config - Client configuration
 * @returns A new KV instance
 */
export function createClient(config?: KVClientConfig): KV {
  return new KV(config);
}
