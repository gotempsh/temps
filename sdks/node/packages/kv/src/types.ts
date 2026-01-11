/**
 * Configuration options for the KV client
 */
export interface KVClientConfig {
  /**
   * Base URL for the Temps API
   * @default process.env.TEMPS_API_URL
   */
  apiUrl?: string;

  /**
   * Authentication token (API key or deployment token)
   * @default process.env.TEMPS_TOKEN
   */
  token?: string;

  /**
   * Project ID for KV operations.
   * Required when using API keys or session authentication.
   * Optional when using deployment tokens (project is inferred from token).
   * @default process.env.TEMPS_PROJECT_ID
   */
  projectId?: number;
}

/**
 * Options for the SET operation
 */
export interface SetOptions {
  /**
   * Set expiration time in seconds
   */
  ex?: number;

  /**
   * Set expiration time in milliseconds
   */
  px?: number;

  /**
   * Only set if key does not exist
   */
  nx?: boolean;

  /**
   * Only set if key already exists
   */
  xx?: boolean;
}

/**
 * Response from the KV API
 */
export interface KVResponse<T = unknown> {
  data?: T;
  error?: {
    message: string;
    code?: string;
  };
}

/**
 * Response from GET operation
 */
export interface GetResponse<T = unknown> {
  value: T | null;
}

/**
 * Response from SET operation
 */
export interface SetResponse {
  result: 'OK' | null;
}

/**
 * Response from DEL operation
 */
export interface DelResponse {
  deleted: number;
}

/**
 * Response from INCR operation
 */
export interface IncrResponse {
  value: number;
}

/**
 * Response from EXPIRE operation
 */
export interface ExpireResponse {
  result: number;
}

/**
 * Response from TTL operation
 */
export interface TtlResponse {
  ttl: number;
}

/**
 * Response from KEYS operation
 */
export interface KeysResponse {
  keys: string[];
}
