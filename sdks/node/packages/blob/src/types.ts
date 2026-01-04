/**
 * Configuration options for the Blob client
 */
export interface BlobClientConfig {
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
   * Project ID for Blob operations.
   * Required when using API keys or session authentication.
   * Optional when using deployment tokens (project is inferred from token).
   * @default process.env.TEMPS_PROJECT_ID
   */
  projectId?: number;
}

/**
 * Options for the PUT (upload) operation
 */
export interface PutOptions {
  /**
   * Content type of the file
   * Will be auto-detected if not provided
   */
  contentType?: string;

  /**
   * Add a random suffix to the filename to prevent collisions
   * @default true
   */
  addRandomSuffix?: boolean;

  /**
   * Custom cache control header
   */
  cacheControl?: string;

  /**
   * Content encoding (e.g., 'gzip')
   */
  contentEncoding?: string;

  /**
   * Content disposition (e.g., 'attachment; filename="file.txt"')
   */
  contentDisposition?: string;
}

/**
 * Information about an uploaded blob
 */
export interface BlobInfo {
  /**
   * Full URL to access the blob
   */
  url: string;

  /**
   * Path/name of the blob
   */
  pathname: string;

  /**
   * MIME type of the blob
   */
  contentType: string;

  /**
   * Size of the blob in bytes
   */
  size: number;

  /**
   * ISO 8601 timestamp when the blob was uploaded
   */
  uploadedAt: string;
}

/**
 * Options for listing blobs
 */
export interface ListOptions {
  /**
   * Maximum number of blobs to return
   * @default 1000
   */
  limit?: number;

  /**
   * Filter blobs by prefix (folder path)
   */
  prefix?: string;

  /**
   * Cursor for pagination (from previous response)
   */
  cursor?: string;
}

/**
 * Result of listing blobs
 */
export interface ListResult {
  /**
   * Array of blob information
   */
  blobs: BlobInfo[];

  /**
   * Cursor for next page (if hasMore is true)
   */
  cursor?: string;

  /**
   * Whether there are more blobs to fetch
   */
  hasMore: boolean;
}

/**
 * Response from PUT operation
 */
export interface PutResponse {
  url: string;
  pathname: string;
  contentType: string;
  size: number;
  uploadedAt: string;
}

/**
 * Response from HEAD operation
 */
export interface HeadResponse {
  url: string;
  pathname: string;
  contentType: string;
  size: number;
  uploadedAt: string;
  cacheControl?: string;
  contentEncoding?: string;
  contentDisposition?: string;
}

/**
 * Response from LIST operation
 */
export interface ListResponse {
  blobs: Array<{
    url: string;
    pathname: string;
    contentType: string;
    size: number;
    uploadedAt: string;
  }>;
  cursor?: string;
  hasMore: boolean;
}

/**
 * Body types that can be uploaded
 */
export type BlobBody =
  | string
  | ArrayBuffer
  | Uint8Array
  | Blob
  | ReadableStream<Uint8Array>
  | Buffer;
