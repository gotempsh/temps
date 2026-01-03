import type {
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
import { BlobError } from './errors.js';

/**
 * Blob client for interacting with the Temps Blob store
 */
export class BlobClient {
  private readonly apiUrl: string;
  private readonly token: string;

  constructor(config: BlobClientConfig = {}) {
    const apiUrl = config.apiUrl || process.env.TEMPS_API_URL;
    const token = config.token || process.env.TEMPS_TOKEN;

    if (!apiUrl) {
      throw BlobError.missingConfig('apiUrl');
    }
    if (!token) {
      throw BlobError.missingConfig('token');
    }

    // Remove trailing slash from API URL
    this.apiUrl = apiUrl.replace(/\/$/, '');
    this.token = token;
  }

  /**
   * Make an authenticated JSON request to the Blob API
   */
  private async jsonRequest<T>(
    method: string,
    endpoint: string,
    body?: Record<string, unknown>
  ): Promise<T> {
    const url = `${this.apiUrl}/api/blob${endpoint}`;

    try {
      const response = await fetch(url, {
        method,
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${this.token}`,
        },
        body: body ? JSON.stringify(body) : undefined,
      });

      const data = await response.json() as T & { error?: { message: string; code?: string } };

      if (!response.ok) {
        throw BlobError.fromResponse(response, data);
      }

      return data;
    } catch (error) {
      if (error instanceof BlobError) {
        throw error;
      }
      throw BlobError.networkError(error as Error);
    }
  }

  /**
   * Upload a blob
   * @param pathname - The path/name for the blob (e.g., 'images/avatar.png')
   * @param body - The content to upload
   * @param options - Upload options
   * @returns Information about the uploaded blob
   */
  async put(pathname: string, body: BlobBody, options?: PutOptions): Promise<BlobInfo> {
    if (!pathname) {
      throw BlobError.invalidInput('pathname is required');
    }

    const url = `${this.apiUrl}/api/blob`;

    try {
      // Create FormData for multipart upload
      const formData = new FormData();

      // Convert body to Blob if needed
      let blobContent: Blob;
      if (typeof body === 'string') {
        blobContent = new Blob([body], { type: options?.contentType || 'text/plain' });
      } else if (body instanceof ArrayBuffer) {
        blobContent = new Blob([body], { type: options?.contentType || 'application/octet-stream' });
      } else if (body instanceof Uint8Array) {
        // Copy to new ArrayBuffer to avoid SharedArrayBuffer and offset issues
        const arrayBuffer = new ArrayBuffer(body.byteLength);
        new Uint8Array(arrayBuffer).set(body);
        blobContent = new Blob([arrayBuffer], { type: options?.contentType || 'application/octet-stream' });
      } else if (body instanceof Blob) {
        blobContent = body;
      } else if (typeof Buffer !== 'undefined' && Buffer.isBuffer(body)) {
        // Buffer extends Uint8Array, copy to new ArrayBuffer
        const arrayBuffer = new ArrayBuffer(body.byteLength);
        new Uint8Array(arrayBuffer).set(body);
        blobContent = new Blob([arrayBuffer], { type: options?.contentType || 'application/octet-stream' });
      } else {
        // ReadableStream - collect into Uint8Array
        const reader = (body as ReadableStream<Uint8Array>).getReader();
        const chunks: Uint8Array[] = [];
        let done = false;
        while (!done) {
          const result = await reader.read();
          done = result.done;
          if (result.value) {
            chunks.push(result.value);
          }
        }
        const totalLength = chunks.reduce((acc, chunk) => acc + chunk.length, 0);
        const combined = new Uint8Array(totalLength);
        let offset = 0;
        for (const chunk of chunks) {
          combined.set(chunk, offset);
          offset += chunk.length;
        }
        blobContent = new Blob([combined.buffer], { type: options?.contentType || 'application/octet-stream' });
      }

      formData.append('file', blobContent, pathname);
      formData.append('pathname', pathname);

      if (options?.contentType) {
        formData.append('contentType', options.contentType);
      }
      if (options?.addRandomSuffix !== undefined) {
        formData.append('addRandomSuffix', String(options.addRandomSuffix));
      }
      if (options?.cacheControl) {
        formData.append('cacheControl', options.cacheControl);
      }
      if (options?.contentEncoding) {
        formData.append('contentEncoding', options.contentEncoding);
      }
      if (options?.contentDisposition) {
        formData.append('contentDisposition', options.contentDisposition);
      }

      const response = await fetch(url, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${this.token}`,
        },
        body: formData,
      });

      const data = await response.json() as PutResponse & { error?: { message: string; code?: string } };

      if (!response.ok) {
        throw BlobError.fromResponse(response, data);
      }

      return {
        url: data.url,
        pathname: data.pathname,
        contentType: data.contentType,
        size: data.size,
        uploadedAt: data.uploadedAt,
      };
    } catch (error) {
      if (error instanceof BlobError) {
        throw error;
      }
      throw BlobError.networkError(error as Error);
    }
  }

  /**
   * Delete one or more blobs
   * @param urls - URL or array of URLs to delete
   */
  async del(urls: string | string[]): Promise<void> {
    const urlArray = Array.isArray(urls) ? urls : [urls];

    if (urlArray.length === 0) {
      return;
    }

    await this.jsonRequest('DELETE', '', { urls: urlArray });
  }

  /**
   * Get metadata for a blob
   * @param url - URL of the blob
   * @returns Blob metadata
   */
  async head(url: string): Promise<BlobInfo> {
    if (!url) {
      throw BlobError.invalidInput('url is required');
    }

    // Extract pathname from URL
    const pathname = this.extractPathname(url);
    const requestUrl = `${this.apiUrl}/api/blob/${pathname}`;

    try {
      const response = await fetch(requestUrl, {
        method: 'HEAD',
        headers: {
          'Authorization': `Bearer ${this.token}`,
        },
      });

      if (!response.ok) {
        if (response.status === 404) {
          throw BlobError.notFound(url);
        }
        throw BlobError.fromResponse(response);
      }

      // Parse headers to extract metadata
      const contentType = response.headers.get('content-type') || 'application/octet-stream';
      const contentLength = response.headers.get('content-length');
      const lastModified = response.headers.get('last-modified');

      return {
        url,
        pathname,
        contentType,
        size: contentLength ? parseInt(contentLength, 10) : 0,
        uploadedAt: lastModified || new Date().toISOString(),
      };
    } catch (error) {
      if (error instanceof BlobError) {
        throw error;
      }
      throw BlobError.networkError(error as Error);
    }
  }

  /**
   * List blobs with pagination
   * @param options - List options (limit, prefix, cursor)
   * @returns List of blobs with pagination info
   */
  async list(options?: ListOptions): Promise<ListResult> {
    const params: Record<string, unknown> = {};

    if (options?.limit !== undefined) {
      params.limit = options.limit;
    }
    if (options?.prefix) {
      params.prefix = options.prefix;
    }
    if (options?.cursor) {
      params.cursor = options.cursor;
    }

    const queryString = new URLSearchParams(
      Object.entries(params).map(([k, v]) => [k, String(v)])
    ).toString();

    const endpoint = queryString ? `?${queryString}` : '';
    const data = await this.jsonRequest<ListResponse>('GET', endpoint);

    return {
      blobs: data.blobs.map((b) => ({
        url: b.url,
        pathname: b.pathname,
        contentType: b.contentType,
        size: b.size,
        uploadedAt: b.uploadedAt,
      })),
      cursor: data.cursor,
      hasMore: data.hasMore,
    };
  }

  /**
   * Download a blob
   * @param url - URL of the blob to download
   * @returns Response object with blob content
   */
  async download(url: string): Promise<Response> {
    if (!url) {
      throw BlobError.invalidInput('url is required');
    }

    const pathname = this.extractPathname(url);
    const requestUrl = `${this.apiUrl}/api/blob/${pathname}`;

    try {
      const response = await fetch(requestUrl, {
        method: 'GET',
        headers: {
          'Authorization': `Bearer ${this.token}`,
        },
      });

      if (!response.ok) {
        if (response.status === 404) {
          throw BlobError.notFound(url);
        }
        const data = await response.json().catch(() => ({})) as { error?: { message: string; code?: string } };
        throw BlobError.fromResponse(response, data);
      }

      return response;
    } catch (error) {
      if (error instanceof BlobError) {
        throw error;
      }
      throw BlobError.networkError(error as Error);
    }
  }

  /**
   * Copy a blob to a new location
   * @param fromUrl - Source blob URL
   * @param toPathname - Destination path
   * @returns Information about the copied blob
   */
  async copy(fromUrl: string, toPathname: string): Promise<BlobInfo> {
    if (!fromUrl) {
      throw BlobError.invalidInput('fromUrl is required');
    }
    if (!toPathname) {
      throw BlobError.invalidInput('toPathname is required');
    }

    const data = await this.jsonRequest<PutResponse>('POST', '/copy', {
      fromUrl,
      toPathname,
    });

    return {
      url: data.url,
      pathname: data.pathname,
      contentType: data.contentType,
      size: data.size,
      uploadedAt: data.uploadedAt,
    };
  }

  /**
   * Extract pathname from a full blob URL
   */
  private extractPathname(url: string): string {
    // Handle both full URLs and relative paths
    if (url.startsWith('http://') || url.startsWith('https://')) {
      try {
        const parsedUrl = new URL(url);
        // Remove /api/blob/ prefix if present
        let pathname = parsedUrl.pathname;
        if (pathname.startsWith('/api/blob/')) {
          pathname = pathname.substring('/api/blob/'.length);
        }
        return pathname;
      } catch {
        // If URL parsing fails, treat as pathname
        return url;
      }
    }

    // Handle relative paths
    if (url.startsWith('/api/blob/')) {
      return url.substring('/api/blob/'.length);
    }
    if (url.startsWith('/')) {
      return url.substring(1);
    }

    return url;
  }
}

/**
 * Create a new Blob client instance
 * @param config - Client configuration
 * @returns A new BlobClient instance
 */
export function createClient(config?: BlobClientConfig): BlobClient {
  return new BlobClient(config);
}
