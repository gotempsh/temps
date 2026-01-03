import { BlobClient, createClient } from './client.js';
import type { BlobClientConfig, PutOptions, ListOptions, BlobBody } from './types.js';

// Re-export everything
export { BlobClient, createClient } from './client.js';
export { BlobError } from './errors.js';
export type {
  BlobClientConfig,
  BlobInfo,
  PutOptions,
  ListOptions,
  ListResult,
  PutResponse,
  HeadResponse,
  ListResponse,
  BlobBody,
} from './types.js';

// Singleton instance for convenience functions
let defaultClient: BlobClient | null = null;

function getDefaultClient(): BlobClient {
  if (!defaultClient) {
    defaultClient = createClient();
  }
  return defaultClient;
}

/**
 * Default Blob instance using environment variables
 * Use this for quick access: `import { blob } from '@temps-sdk/blob'`
 */
export const blob = {
  put: (pathname: string, body: BlobBody, options?: PutOptions) =>
    getDefaultClient().put(pathname, body, options),
  del: (urls: string | string[]) => getDefaultClient().del(urls),
  head: (url: string) => getDefaultClient().head(url),
  list: (options?: ListOptions) => getDefaultClient().list(options),
  download: (url: string) => getDefaultClient().download(url),
  copy: (fromUrl: string, toPathname: string) => getDefaultClient().copy(fromUrl, toPathname),
};

// Convenience functions that use the default client

/**
 * Upload a blob
 * @param pathname - The path/name for the blob
 * @param body - The content to upload
 * @param options - Upload options
 */
export async function put(pathname: string, body: BlobBody, options?: PutOptions) {
  return getDefaultClient().put(pathname, body, options);
}

/**
 * Delete one or more blobs
 * @param urls - URL or array of URLs to delete
 */
export async function del(urls: string | string[]) {
  return getDefaultClient().del(urls);
}

/**
 * Get metadata for a blob
 * @param url - URL of the blob
 */
export async function head(url: string) {
  return getDefaultClient().head(url);
}

/**
 * List blobs with pagination
 * @param options - List options
 */
export async function list(options?: ListOptions) {
  return getDefaultClient().list(options);
}

/**
 * Download a blob
 * @param url - URL of the blob
 */
export async function download(url: string) {
  return getDefaultClient().download(url);
}

/**
 * Copy a blob to a new location
 * @param fromUrl - Source blob URL
 * @param toPathname - Destination path
 */
export async function copy(fromUrl: string, toPathname: string) {
  return getDefaultClient().copy(fromUrl, toPathname);
}

/**
 * Reset the default client (useful for testing)
 * @internal
 */
export function _resetDefaultClient(): void {
  defaultClient = null;
}
