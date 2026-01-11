import { vi, beforeEach, afterEach } from 'vitest';

// Store original env
const originalEnv = { ...process.env };

// Mock fetch globally
export const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

// Helper to create mock responses
export function createMockResponse<T>(
  data: T,
  options: { ok?: boolean; status?: number; headers?: Record<string, string> } = {}
): Response {
  const { ok = true, status = 200, headers = {} } = options;
  const responseHeaders = new Headers(headers);

  return {
    ok,
    status,
    headers: responseHeaders,
    json: () => Promise.resolve(data),
    text: () => Promise.resolve(JSON.stringify(data)),
    blob: () => Promise.resolve(new Blob([JSON.stringify(data)])),
    arrayBuffer: () => Promise.resolve(new ArrayBuffer(0)),
    formData: () => Promise.resolve(new FormData()),
    redirected: false,
    statusText: ok ? 'OK' : 'Error',
    type: 'basic',
    url: '',
    clone: function() { return createMockResponse(data, options); },
    body: null,
    bodyUsed: false,
    bytes: () => Promise.resolve(new Uint8Array()),
  } as Response;
}

// Helper to create error responses
export function createErrorResponse(
  message: string,
  code?: string,
  status = 400
): Response {
  return createMockResponse(
    { error: { message, code } },
    { ok: false, status }
  );
}

// Helper to create HEAD response with headers
export function createHeadResponse(headers: Record<string, string>, ok = true, status = 200): Response {
  return createMockResponse({}, { ok, status, headers });
}

// Reset state before each test
beforeEach(() => {
  mockFetch.mockReset();
  // Set default test environment variables
  process.env.TEMPS_API_URL = 'https://api.temps.test';
  process.env.TEMPS_TOKEN = 'test-token-12345';
});

// Restore original env after each test
afterEach(() => {
  process.env = { ...originalEnv };
});

// Export test constants
export const TEST_API_URL = 'https://api.temps.test';
export const TEST_TOKEN = 'test-token-12345';
