import { describe, it, expect, beforeEach } from 'vitest';
import {
  mockFetch,
  createMockResponse,
  createErrorResponse,
  createHeadResponse,
  TEST_API_URL,
  TEST_TOKEN,
} from './setup.js';
import {
  BlobClient,
  createClient,
  blob,
  put,
  del,
  head,
  list,
  download,
  copy,
  _resetDefaultClient,
  BlobError,
} from '../index.js';

describe('BlobClient', () => {
  describe('constructor', () => {
    it('should create client with environment variables', () => {
      const client = new BlobClient();
      expect(client).toBeInstanceOf(BlobClient);
    });

    it('should create client with explicit config', () => {
      const client = new BlobClient({
        apiUrl: 'https://custom.api.test',
        token: 'custom-token',
      });
      expect(client).toBeInstanceOf(BlobClient);
    });

    it('should throw when apiUrl is missing', () => {
      delete process.env.TEMPS_API_URL;
      expect(() => new BlobClient()).toThrow(BlobError);
      expect(() => new BlobClient()).toThrow('Missing required configuration: apiUrl');
    });

    it('should throw when token is missing', () => {
      delete process.env.TEMPS_TOKEN;
      expect(() => new BlobClient()).toThrow(BlobError);
      expect(() => new BlobClient()).toThrow('Missing required configuration: token');
    });

    it('should remove trailing slash from apiUrl', () => {
      const client = new BlobClient({
        apiUrl: 'https://api.test/',
        token: 'token',
      });
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          blobs: [],
          hasMore: false,
        })
      );
      client.list();
      expect(mockFetch).toHaveBeenCalledWith(
        'https://api.test/api/blob',
        expect.any(Object)
      );
    });
  });

  describe('createClient', () => {
    it('should create a new BlobClient instance', () => {
      const client = createClient();
      expect(client).toBeInstanceOf(BlobClient);
    });

    it('should accept config options', () => {
      const client = createClient({
        apiUrl: 'https://custom.api.test',
        token: 'custom-token',
      });
      expect(client).toBeInstanceOf(BlobClient);
    });
  });

  describe('put', () => {
    it('should upload a string blob', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: `${TEST_API_URL}/api/blob/test.txt`,
          pathname: 'test.txt',
          contentType: 'text/plain',
          size: 13,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );

      const result = await client.put('test.txt', 'Hello, World!');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob`,
        expect.objectContaining({
          method: 'POST',
          headers: expect.objectContaining({
            'Authorization': `Bearer ${TEST_TOKEN}`,
          }),
        })
      );
      expect(result.pathname).toBe('test.txt');
      expect(result.contentType).toBe('text/plain');
      expect(result.size).toBe(13);
    });

    it('should upload with custom content type', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: `${TEST_API_URL}/api/blob/image.png`,
          pathname: 'image.png',
          contentType: 'image/png',
          size: 1024,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );

      const result = await client.put('image.png', new Uint8Array(1024), {
        contentType: 'image/png',
      });

      expect(result.contentType).toBe('image/png');
    });

    it('should upload with options', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: `${TEST_API_URL}/api/blob/file-abc123.txt`,
          pathname: 'file-abc123.txt',
          contentType: 'text/plain',
          size: 100,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );

      await client.put('file.txt', 'content', {
        addRandomSuffix: true,
        cacheControl: 'max-age=31536000',
        contentDisposition: 'attachment',
      });

      // Verify FormData was sent with options
      expect(mockFetch).toHaveBeenCalled();
      const [, fetchOptions] = mockFetch.mock.calls[0];
      expect(fetchOptions.body).toBeInstanceOf(FormData);
    });

    it('should throw on empty pathname', async () => {
      const client = new BlobClient();
      await expect(client.put('', 'content')).rejects.toThrow(BlobError);
      await expect(client.put('', 'content')).rejects.toThrow('pathname is required');
    });

    it('should upload ArrayBuffer', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: `${TEST_API_URL}/api/blob/binary.bin`,
          pathname: 'binary.bin',
          contentType: 'application/octet-stream',
          size: 256,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );

      const result = await client.put('binary.bin', new ArrayBuffer(256));

      expect(result.pathname).toBe('binary.bin');
      expect(result.size).toBe(256);
    });

    it('should upload Blob', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: `${TEST_API_URL}/api/blob/data.json`,
          pathname: 'data.json',
          contentType: 'application/json',
          size: 50,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );

      const blobData = new Blob(['{"key": "value"}'], { type: 'application/json' });
      const result = await client.put('data.json', blobData);

      expect(result.pathname).toBe('data.json');
    });

    it('should handle upload errors', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createErrorResponse('Upload failed', 'UPLOAD_ERROR', 500)
      );

      try {
        await client.put('file.txt', 'content');
        expect.fail('Expected BlobError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(BlobError);
        expect((error as BlobError).message).toBe('Upload failed');
        expect((error as BlobError).code).toBe('UPLOAD_ERROR');
        expect((error as BlobError).status).toBe(500);
      }
    });
  });

  describe('del', () => {
    it('should delete a single blob', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(createMockResponse({}));

      await client.del(`${TEST_API_URL}/api/blob/test.txt`);

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob`,
        expect.objectContaining({
          method: 'DELETE',
          headers: expect.objectContaining({
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          }),
          body: JSON.stringify({ urls: [`${TEST_API_URL}/api/blob/test.txt`] }),
        })
      );
    });

    it('should delete multiple blobs', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(createMockResponse({}));

      const urls = [
        `${TEST_API_URL}/api/blob/file1.txt`,
        `${TEST_API_URL}/api/blob/file2.txt`,
        `${TEST_API_URL}/api/blob/file3.txt`,
      ];

      await client.del(urls);

      expect(mockFetch).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          body: JSON.stringify({ urls }),
        })
      );
    });

    it('should handle empty array gracefully', async () => {
      const client = new BlobClient();

      await client.del([]);

      expect(mockFetch).not.toHaveBeenCalled();
    });

    it('should handle delete errors', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createErrorResponse('Delete failed', 'DELETE_ERROR', 500)
      );

      await expect(client.del('url')).rejects.toThrow(BlobError);
    });
  });

  describe('head', () => {
    it('should get blob metadata', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createHeadResponse({
          'content-type': 'image/png',
          'content-length': '1024',
          'last-modified': '2025-01-03T12:00:00Z',
        })
      );

      const result = await client.head(`${TEST_API_URL}/api/blob/image.png`);

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob/image.png`,
        expect.objectContaining({
          method: 'HEAD',
          headers: expect.objectContaining({
            'Authorization': `Bearer ${TEST_TOKEN}`,
          }),
        })
      );
      expect(result.contentType).toBe('image/png');
      expect(result.size).toBe(1024);
    });

    it('should throw on empty url', async () => {
      const client = new BlobClient();
      await expect(client.head('')).rejects.toThrow(BlobError);
      await expect(client.head('')).rejects.toThrow('url is required');
    });

    it('should throw not found error', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(createHeadResponse({}, false, 404));

      try {
        await client.head('nonexistent.txt');
        expect.fail('Expected BlobError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(BlobError);
        expect((error as BlobError).code).toBe('NOT_FOUND');
        expect((error as BlobError).status).toBe(404);
      }
    });

    it('should extract pathname from full URL', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createHeadResponse({
          'content-type': 'text/plain',
          'content-length': '100',
        })
      );

      await client.head('https://api.temps.test/api/blob/folder/file.txt');

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob/folder/file.txt`,
        expect.any(Object)
      );
    });
  });

  describe('list', () => {
    it('should list blobs', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          blobs: [
            {
              url: `${TEST_API_URL}/api/blob/file1.txt`,
              pathname: 'file1.txt',
              contentType: 'text/plain',
              size: 100,
              uploadedAt: '2025-01-03T12:00:00Z',
            },
            {
              url: `${TEST_API_URL}/api/blob/file2.txt`,
              pathname: 'file2.txt',
              contentType: 'text/plain',
              size: 200,
              uploadedAt: '2025-01-03T12:01:00Z',
            },
          ],
          hasMore: false,
        })
      );

      const result = await client.list();

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob`,
        expect.objectContaining({
          method: 'GET',
        })
      );
      expect(result.blobs).toHaveLength(2);
      expect(result.hasMore).toBe(false);
    });

    it('should list blobs with prefix', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          blobs: [],
          hasMore: false,
        })
      );

      await client.list({ prefix: 'images/' });

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob?prefix=images%2F`,
        expect.any(Object)
      );
    });

    it('should list blobs with limit', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          blobs: [],
          hasMore: true,
          cursor: 'next-cursor',
        })
      );

      const result = await client.list({ limit: 10 });

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob?limit=10`,
        expect.any(Object)
      );
      expect(result.hasMore).toBe(true);
      expect(result.cursor).toBe('next-cursor');
    });

    it('should list blobs with cursor', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          blobs: [],
          hasMore: false,
        })
      );

      await client.list({ cursor: 'some-cursor' });

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob?cursor=some-cursor`,
        expect.any(Object)
      );
    });

    it('should combine multiple options', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          blobs: [],
          hasMore: false,
        })
      );

      await client.list({ limit: 50, prefix: 'docs/', cursor: 'abc' });

      const url = mockFetch.mock.calls[0][0] as string;
      expect(url).toContain('limit=50');
      expect(url).toContain('prefix=docs%2F');
      expect(url).toContain('cursor=abc');
    });
  });

  describe('download', () => {
    it('should download a blob', async () => {
      const client = new BlobClient();
      const mockResponse = createMockResponse('file content');
      mockFetch.mockResolvedValueOnce(mockResponse);

      const result = await client.download(`${TEST_API_URL}/api/blob/file.txt`);

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob/file.txt`,
        expect.objectContaining({
          method: 'GET',
          headers: expect.objectContaining({
            'Authorization': `Bearer ${TEST_TOKEN}`,
          }),
        })
      );
      expect(result).toBe(mockResponse);
    });

    it('should throw on empty url', async () => {
      const client = new BlobClient();
      await expect(client.download('')).rejects.toThrow(BlobError);
      await expect(client.download('')).rejects.toThrow('url is required');
    });

    it('should throw not found error', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(createErrorResponse('Not found', 'NOT_FOUND', 404));

      try {
        await client.download('nonexistent.txt');
        expect.fail('Expected BlobError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(BlobError);
        expect((error as BlobError).code).toBe('NOT_FOUND');
        expect((error as BlobError).status).toBe(404);
      }
    });
  });

  describe('copy', () => {
    it('should copy a blob', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: `${TEST_API_URL}/api/blob/copied.txt`,
          pathname: 'copied.txt',
          contentType: 'text/plain',
          size: 100,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );

      const result = await client.copy(
        `${TEST_API_URL}/api/blob/original.txt`,
        'copied.txt'
      );

      expect(mockFetch).toHaveBeenCalledWith(
        `${TEST_API_URL}/api/blob/copy`,
        expect.objectContaining({
          method: 'POST',
          headers: expect.objectContaining({
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${TEST_TOKEN}`,
          }),
          body: JSON.stringify({
            fromUrl: `${TEST_API_URL}/api/blob/original.txt`,
            toPathname: 'copied.txt',
          }),
        })
      );
      expect(result.pathname).toBe('copied.txt');
    });

    it('should throw on empty fromUrl', async () => {
      const client = new BlobClient();
      await expect(client.copy('', 'dest.txt')).rejects.toThrow(BlobError);
      await expect(client.copy('', 'dest.txt')).rejects.toThrow('fromUrl is required');
    });

    it('should throw on empty toPathname', async () => {
      const client = new BlobClient();
      await expect(client.copy('source.txt', '')).rejects.toThrow(BlobError);
      await expect(client.copy('source.txt', '')).rejects.toThrow('toPathname is required');
    });
  });

  describe('error handling', () => {
    it('should handle network errors', async () => {
      const client = new BlobClient();
      mockFetch.mockRejectedValueOnce(new Error('Network failure'));

      try {
        await client.list();
        expect.fail('Expected BlobError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(BlobError);
        expect((error as BlobError).code).toBe('NETWORK_ERROR');
        expect((error as BlobError).message).toContain('Network failure');
      }
    });

    it('should handle server errors', async () => {
      const client = new BlobClient();
      mockFetch.mockResolvedValueOnce(
        createErrorResponse('Internal server error', 'INTERNAL_ERROR', 500)
      );

      try {
        await client.list();
        expect.fail('Expected BlobError to be thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(BlobError);
        expect((error as BlobError).status).toBe(500);
        expect((error as BlobError).code).toBe('INTERNAL_ERROR');
      }
    });
  });
});

describe('Convenience Functions', () => {
  beforeEach(() => {
    _resetDefaultClient();
  });

  describe('blob object', () => {
    it('should put using blob.put', async () => {
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: 'test-url',
          pathname: 'test.txt',
          contentType: 'text/plain',
          size: 4,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );
      const result = await blob.put('test.txt', 'test');
      expect(result.pathname).toBe('test.txt');
    });

    it('should delete using blob.del', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({}));
      await blob.del('url');
      expect(mockFetch).toHaveBeenCalled();
    });

    it('should get metadata using blob.head', async () => {
      mockFetch.mockResolvedValueOnce(
        createHeadResponse({
          'content-type': 'text/plain',
          'content-length': '100',
        })
      );
      const result = await blob.head('file.txt');
      expect(result.contentType).toBe('text/plain');
    });

    it('should list using blob.list', async () => {
      mockFetch.mockResolvedValueOnce(
        createMockResponse({ blobs: [], hasMore: false })
      );
      const result = await blob.list();
      expect(result.blobs).toEqual([]);
    });

    it('should download using blob.download', async () => {
      const mockResponse = createMockResponse('content');
      mockFetch.mockResolvedValueOnce(mockResponse);
      const result = await blob.download('file.txt');
      expect(result).toBe(mockResponse);
    });

    it('should copy using blob.copy', async () => {
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: 'new-url',
          pathname: 'dest.txt',
          contentType: 'text/plain',
          size: 100,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );
      const result = await blob.copy('source.txt', 'dest.txt');
      expect(result.pathname).toBe('dest.txt');
    });
  });

  describe('standalone functions', () => {
    it('should put using put()', async () => {
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: 'test-url',
          pathname: 'file.txt',
          contentType: 'text/plain',
          size: 7,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );
      const result = await put('file.txt', 'content');
      expect(result.pathname).toBe('file.txt');
    });

    it('should delete using del()', async () => {
      mockFetch.mockResolvedValueOnce(createMockResponse({}));
      await del(['url1', 'url2']);
      expect(mockFetch).toHaveBeenCalled();
    });

    it('should get metadata using head()', async () => {
      mockFetch.mockResolvedValueOnce(
        createHeadResponse({
          'content-type': 'application/pdf',
          'content-length': '5000',
        })
      );
      const result = await head('document.pdf');
      expect(result.contentType).toBe('application/pdf');
    });

    it('should list using list()', async () => {
      mockFetch.mockResolvedValueOnce(
        createMockResponse({ blobs: [], hasMore: true, cursor: 'abc' })
      );
      const result = await list({ limit: 10 });
      expect(result.hasMore).toBe(true);
    });

    it('should download using download()', async () => {
      const mockResponse = createMockResponse('data');
      mockFetch.mockResolvedValueOnce(mockResponse);
      const result = await download('data.bin');
      expect(result).toBe(mockResponse);
    });

    it('should copy using copy()', async () => {
      mockFetch.mockResolvedValueOnce(
        createMockResponse({
          url: 'copied-url',
          pathname: 'backup.txt',
          contentType: 'text/plain',
          size: 50,
          uploadedAt: '2025-01-03T12:00:00Z',
        })
      );
      const result = await copy('original.txt', 'backup.txt');
      expect(result.pathname).toBe('backup.txt');
    });
  });

  describe('singleton behavior', () => {
    it('should reuse the same client instance', async () => {
      mockFetch.mockResolvedValue(createMockResponse({ blobs: [], hasMore: false }));

      await blob.list();
      await blob.list();

      expect(mockFetch).toHaveBeenCalledTimes(2);
      const [, firstCall] = mockFetch.mock.calls[0];
      const [, secondCall] = mockFetch.mock.calls[1];
      expect(firstCall.headers.Authorization).toBe(secondCall.headers.Authorization);
    });

    it('should reset default client', async () => {
      mockFetch.mockResolvedValue(createMockResponse({ blobs: [], hasMore: false }));

      await blob.list();
      _resetDefaultClient();

      process.env.TEMPS_TOKEN = 'new-token';

      await blob.list();

      const [, secondCall] = mockFetch.mock.calls[1];
      expect(secondCall.headers.Authorization).toBe('Bearer new-token');
    });
  });
});

describe('BlobError', () => {
  it('should create error with message only', () => {
    const error = new BlobError('Something went wrong');
    expect(error.message).toBe('Something went wrong');
    expect(error.name).toBe('BlobError');
    expect(error.code).toBeUndefined();
    expect(error.status).toBeUndefined();
  });

  it('should create error with code', () => {
    const error = new BlobError('Not found', { code: 'NOT_FOUND' });
    expect(error.message).toBe('Not found');
    expect(error.code).toBe('NOT_FOUND');
  });

  it('should create error with status', () => {
    const error = new BlobError('Unauthorized', { code: 'UNAUTHORIZED', status: 401 });
    expect(error.message).toBe('Unauthorized');
    expect(error.code).toBe('UNAUTHORIZED');
    expect(error.status).toBe(401);
  });

  it('should create from response', () => {
    const response = { ok: false, status: 404 } as Response;
    const body = { error: { message: 'Blob not found', code: 'BLOB_NOT_FOUND' } };

    const error = BlobError.fromResponse(response, body);
    expect(error.message).toBe('Blob not found');
    expect(error.code).toBe('BLOB_NOT_FOUND');
    expect(error.status).toBe(404);
  });

  it('should create from response without body', () => {
    const response = { ok: false, status: 500 } as Response;

    const error = BlobError.fromResponse(response);
    expect(error.message).toBe('Blob operation failed with status 500');
    expect(error.status).toBe(500);
  });

  it('should create missing config error for apiUrl', () => {
    const error = BlobError.missingConfig('apiUrl');
    expect(error.message).toContain('TEMPS_API_URL');
    expect(error.code).toBe('MISSING_CONFIG');
  });

  it('should create missing config error for token', () => {
    const error = BlobError.missingConfig('token');
    expect(error.message).toContain('TEMPS_TOKEN');
    expect(error.code).toBe('MISSING_CONFIG');
  });

  it('should create network error', () => {
    const originalError = new Error('Connection refused');
    const error = BlobError.networkError(originalError);
    expect(error.message).toContain('Connection refused');
    expect(error.code).toBe('NETWORK_ERROR');
  });

  it('should create not found error', () => {
    const error = BlobError.notFound('/path/to/file.txt');
    expect(error.message).toContain('/path/to/file.txt');
    expect(error.code).toBe('NOT_FOUND');
    expect(error.status).toBe(404);
  });

  it('should create invalid input error', () => {
    const error = BlobError.invalidInput('pathname is required');
    expect(error.message).toBe('pathname is required');
    expect(error.code).toBe('INVALID_INPUT');
    expect(error.status).toBe(400);
  });

  it('should be instanceof Error', () => {
    const error = new BlobError('Test');
    expect(error).toBeInstanceOf(Error);
    expect(error).toBeInstanceOf(BlobError);
  });
});
