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
  private readonly projectId?: number;

  constructor(config: BlobClientConfig = {}) {
    const apiUrl = config.apiUrl || process.env.TEMPS_API_URL;
    const token = config.token || process.env.TEMPS_TOKEN;
    const projectId = config.projectId ?? (process.env.TEMPS_PROJECT_ID ? parseInt(process.env.TEMPS_PROJECT_ID, 10) : undefined);

    if (!apiUrl) {
      throw BlobError.missingConfig('apiUrl');
    }
    if (!token) {
      throw BlobError.missingConfig('token');
    }

    // Remove trailing slash from API URL
    this.apiUrl = apiUrl.replace(/\/$/, '');
    this.token = token;
    this.projectId = projectId;
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

    // For GET/HEAD requests, don't include a body (use query params instead)
    const isBodyAllowed = !['GET', 'HEAD', 'OPTIONS'].includes(method.toUpperCase());

    // Include projectId in body when set (only for non-GET methods)
    const requestBody = isBodyAllowed
      ? (body
          ? (this.projectId !== undefined
              ? { ...body, projectId: this.projectId }
              : body)
          : (this.projectId !== undefined
              ? { projectId: this.projectId }
              : undefined))
      : undefined;

    try {
      const response = await fetch(url, {
        method,
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${this.token}`,
        },
        body: requestBody ? JSON.stringify(requestBody) : undefined,
      });

      // Try to parse response as JSON, but handle non-JSON responses gracefully
      let data: T & { error?: { message: string; code?: string } };
      const responseText = await response.text();

      try {
        data = JSON.parse(responseText) as T & { error?: { message: string; code?: string } };
      } catch {
        // If JSON parsing fails, create an error with the raw response
        if (!response.ok) {
          throw new BlobError(
            `Request failed with status ${response.status}: ${responseText || response.statusText}`,
            { status: response.status }
          );
        }
        throw new BlobError(
          `Failed to parse response as JSON: ${responseText.substring(0, 200)}`,
          { code: 'PARSE_ERROR' }
        );
      }

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

    // Build query params including project_id if set
    const queryParams = new URLSearchParams();
    queryParams.set('pathname', pathname);
    if (options?.contentType) {
      queryParams.set('content_type', options.contentType);
    }
    if (options?.addRandomSuffix !== undefined) {
      queryParams.set('add_random_suffix', String(options.addRandomSuffix));
    }
    if (this.projectId !== undefined) {
      queryParams.set('project_id', String(this.projectId));
    }

    const url = `${this.apiUrl}/api/blob?${queryParams.toString()}`;

    try {
      // Determine content type
      const contentType = options?.contentType || this.guessContentType(pathname);

      // Convert body to raw bytes for upload (server expects raw bytes, not FormData)
      let blobContent: Blob | ArrayBuffer | Uint8Array | string;
      if (typeof body === 'string') {
        blobContent = body;
      } else if (body instanceof ArrayBuffer) {
        blobContent = body;
      } else if (body instanceof Uint8Array) {
        // Copy to new ArrayBuffer to avoid SharedArrayBuffer and offset issues
        const arrayBuffer = new ArrayBuffer(body.byteLength);
        new Uint8Array(arrayBuffer).set(body);
        blobContent = arrayBuffer;
      } else if (body instanceof Blob) {
        blobContent = body;
      } else if (typeof Buffer !== 'undefined' && Buffer.isBuffer(body)) {
        // Buffer extends Uint8Array, copy to new ArrayBuffer
        const arrayBuffer = new ArrayBuffer(body.byteLength);
        new Uint8Array(arrayBuffer).set(body);
        blobContent = arrayBuffer;
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
        blobContent = combined.buffer;
      }

      // Send raw binary body with Content-Type header
      const response = await fetch(url, {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${this.token}`,
          'Content-Type': contentType,
        },
        body: blobContent,
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
   * @param urls - URL or array of URLs/pathnames to delete
   */
  async del(urls: string | string[]): Promise<void> {
    const urlArray = Array.isArray(urls) ? urls : [urls];

    if (urlArray.length === 0) {
      return;
    }

    // Extract pathnames from URLs
    const pathnames = urlArray.map((url) => this.extractPathname(url));

    await this.jsonRequest('DELETE', '', { pathnames });
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
    // Include project_id in path when set: /api/blob/{project_id}/{pathname}
    const requestUrl = this.projectId !== undefined
      ? `${this.apiUrl}/api/blob/${this.projectId}/${pathname}`
      : `${this.apiUrl}/api/blob/${pathname}`;

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
    // Include project_id when set
    if (this.projectId !== undefined) {
      params.project_id = this.projectId;
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
    // Include project_id in path when set: /api/blob/{project_id}/{pathname}
    const requestUrl = this.projectId !== undefined
      ? `${this.apiUrl}/api/blob/${this.projectId}/${pathname}`
      : `${this.apiUrl}/api/blob/${pathname}`;

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
   * Guess content type from pathname extension
   */
  private guessContentType(pathname: string): string {
    const extension = pathname.split('.').pop()?.toLowerCase() || '';

    const mimeTypes: Record<string, string> = {
      // Images
      jpg: 'image/jpeg',
      jpeg: 'image/jpeg',
      png: 'image/png',
      gif: 'image/gif',
      webp: 'image/webp',
      svg: 'image/svg+xml',
      ico: 'image/x-icon',
      // Documents
      pdf: 'application/pdf',
      doc: 'application/msword',
      docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
      xls: 'application/vnd.ms-excel',
      xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      // Text
      txt: 'text/plain',
      html: 'text/html',
      htm: 'text/html',
      css: 'text/css',
      js: 'application/javascript',
      json: 'application/json',
      xml: 'application/xml',
      // Archives
      zip: 'application/zip',
      tar: 'application/x-tar',
      gz: 'application/gzip',
      gzip: 'application/gzip',
      // Media
      mp3: 'audio/mpeg',
      mp4: 'video/mp4',
      webm: 'video/webm',
    };

    return mimeTypes[extension] || 'application/octet-stream';
  }

  /**
   * Extract pathname from a full blob URL
   * Returns the path WITHOUT project_id prefix (e.g., "example/file.txt")
   */
  private extractPathname(url: string): string {
    let path: string;

    // Handle both full URLs and relative paths
    if (url.startsWith('http://') || url.startsWith('https://')) {
      try {
        const parsedUrl = new URL(url);
        path = parsedUrl.pathname;
      } catch {
        // If URL parsing fails, treat as pathname
        path = url;
      }
    } else {
      path = url;
    }

    // Remove /api/blob/ prefix if present
    if (path.startsWith('/api/blob/')) {
      path = path.substring('/api/blob/'.length);
    }

    // Remove leading slash if present
    if (path.startsWith('/')) {
      path = path.substring(1);
    }

    // Remove project_id prefix if present (e.g., "10/example/file.txt" -> "example/file.txt")
    // The URL format from server is /api/blob/{project_id}/{pathname}
    const slashIndex = path.indexOf('/');
    if (slashIndex > 0) {
      const potentialProjectId = path.substring(0, slashIndex);
      // Check if it's a numeric project ID
      if (/^\d+$/.test(potentialProjectId)) {
        path = path.substring(slashIndex + 1);
      }
    }

    return path;
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
