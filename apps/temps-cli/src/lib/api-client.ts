// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '../api/client.gen.js'
import { config, credentials } from '../config/store.js'
import type { UserResponse } from '../api/types.gen.js'

/**
 * Setup the API client with the correct base URL and auth headers
 */
export function normalizeApiUrl(url: string): string {
  // Remove trailing slash
  let normalized = url.replace(/\/+$/, '')
  // Ensure /api suffix if not already present
  if (!normalized.endsWith('/api')) {
    normalized += '/api'
  }
  return normalized
}

/**
 * Module-level pin for the API key the auth interceptor must send. When set
 * (via `setupClient(url, key)`), it takes precedence over `credentials.getApiKey()`.
 *
 * This exists for the api-key login path. `getApiKey()` resolves in priority
 * order (env var > active context > stored secrets), so when an active context
 * already holds a key, a freshly-supplied `--api-key` written to the secrets
 * layer is *shadowed* and never sent — the CLI then validates the wrong key and
 * reports "Invalid API key". Pinning the key the caller actually passed makes
 * `temps login --api-key=…` validate exactly that key, independent of ambient
 * env vars or the active context.
 */
let pinnedApiKey: string | undefined

let interceptorRegistered = false

/**
 * The reply did not come from a Temps API: the URL is wrong (a console page
 * or a captive portal answering 200), the route does not exist on this
 * server, or the host could not be reached at all.
 *
 * Distinct from an error body the server produced itself (problem+json), so
 * callers such as `login` can show it instead of a generic "Invalid API key".
 */
export class ApiResponseError extends Error {
  constructor(
    message: string,
    public readonly status?: number,
    public readonly url?: string,
    options?: { cause?: unknown },
  ) {
    super(message, options)
    this.name = 'ApiResponseError'
  }
}

const BODY_EXCERPT_CHARS = 200

/** A decoded `GET /user/me` reply: an object that carries the user's id. */
export function isUserObject(value: unknown): value is UserResponse {
  return (
    typeof value === 'object' &&
    value !== null &&
    'id' in value &&
    typeof value.id === 'number'
  )
}

/**
 * Narrow a decoded reply to the shape the endpoint is documented to return.
 *
 * `assertTempsApiResponse` catches a reply of the wrong *kind* (a page, an
 * empty body); this catches a JSON document of another shape, which would
 * otherwise read as "no results" through `data?.field ?? []`.
 */
export function requireApiShape<T>(
  data: unknown,
  operation: string,
  isShape: (value: unknown) => value is T,
): T {
  if (!isShape(data)) {
    throw new ApiResponseError(
      `Unexpected reply while ${operation}: the server did not return a Temps API response. Check the URL.`,
    )
  }
  return data
}

function isJsonContentType(contentType: string): boolean {
  const mediaType = contentType.split(';')[0]?.trim().toLowerCase() ?? ''
  return mediaType === 'application/json' || mediaType.endsWith('+json')
}

function isHtmlContentType(contentType: string): boolean {
  const mediaType = contentType.split(';')[0]?.trim().toLowerCase() ?? ''
  return mediaType === 'text/html' || mediaType === 'application/xhtml+xml'
}

/**
 * Response interceptor: reject a 2xx that cannot be a Temps API answer.
 *
 * The Temps API answers every JSON endpoint with a JSON content type, and a
 * real console answers any non-`/api` path with its HTML page and an unknown
 * API route with an empty 404. The generated client would otherwise hand an
 * HTML page back as `data`, and every command reading `data?.field` would
 * print an empty list and exit 0.
 *
 * Conservative on purpose: only HTML, and a GET that returned nothing with no
 * content type, are rejected. Downloads, text and event streams keep working.
 */
export async function assertTempsApiResponse(
  response: Response,
  request: Request,
  options?: { parseAs?: string },
): Promise<Response> {
  if (!response.ok || response.status === 204 || response.status === 205) {
    return response
  }

  // A caller that explicitly asked for text, a blob or a stream knows the
  // reply is not JSON; only the default ('auto') is judged.
  if (options?.parseAs && options.parseAs !== 'auto' && options.parseAs !== 'json') {
    return response
  }

  const contentType = response.headers.get('content-type') ?? ''
  if (isJsonContentType(contentType)) {
    return response
  }

  const where = `The server at ${request.url}`
  const status = `HTTP ${response.status}`
  const hint = 'Check the URL: it must be the address of the Temps server, not a page of its console.'

  if (isHtmlContentType(contentType)) {
    throw new ApiResponseError(
      `${where} did not return a Temps API response (${status}, ${contentType.split(';')[0]?.trim()}). ${hint}`,
      response.status,
      request.url,
    )
  }

  if (!contentType && request.method === 'GET') {
    // Only an untyped reply is looked at, and only until its first chunk: the
    // body may be large or long-lived, so it is never read to the end.
    if (await hasEmptyBody(response)) {
      throw new ApiResponseError(
        `${where} returned an empty body (${status}) where a Temps API response was expected. ${hint}`,
        response.status,
        request.url,
      )
    }
  }

  return response
}

/** True when the reply carries no bytes. Reads at most one chunk of a clone. */
async function hasEmptyBody(response: Response): Promise<boolean> {
  if (response.headers.get('content-length') === '0') return true
  const body = response.clone().body
  if (!body) return true
  const reader = body.getReader()
  try {
    // An empty body closes without yielding a chunk. Only chunk-less bodies
    // are empty; a zero-length chunk is skipped until real data or the end.
    for (;;) {
      const chunk = await reader.read()
      if (chunk.done) return true
      if (chunk.value && chunk.value.byteLength > 0) return false
    }
  } finally {
    void reader.cancel().catch(() => undefined)
  }
}

function errorCauseText(error: Error): string {
  const cause = (error as { cause?: unknown }).cause
  if (cause && typeof cause === 'object') {
    const code = 'code' in cause && typeof cause.code === 'string' ? cause.code : undefined
    const message = 'message' in cause && typeof cause.message === 'string' ? cause.message : undefined
    if (code && message && !message.includes(code)) return `${code}: ${message}`
    if (message) return message
    if (code) return code
  }
  return error.message
}

function isBlankError(error: unknown): boolean {
  if (error === undefined || error === null || error === '') return true
  if (typeof error === 'string') return error.trim() === ''
  if (typeof error === 'object' && !(error instanceof Error)) {
    return Object.keys(error as object).length === 0
  }
  return false
}

/**
 * Error interceptor: say which HTTP status and URL failed when the body gives
 * no message of its own, and name the cause of a transport failure.
 *
 * Structured error bodies (problem+json, `{ message }`) are left untouched so
 * the server's own explanation is still what the user reads.
 */
export function describeApiFailure(
  error: unknown,
  response: Response | undefined,
  request: Request | undefined,
): unknown {
  if (error instanceof ApiResponseError) return error

  const url = request?.url ?? response?.url ?? 'the server'

  if (!response) {
    // An aborted or timed-out request says nothing about whether the host is
    // reachable; report it as what it is instead of "unable to reach".
    if (error instanceof Error && error.name === 'TimeoutError') {
      return new ApiResponseError(`Request to ${url} timed out`, undefined, url, {
        cause: error,
      })
    }
    if (error instanceof Error && error.name === 'AbortError') {
      return error
    }
    if (error instanceof Error) {
      return new ApiResponseError(
        `Unable to reach ${url}: ${errorCauseText(error)}`,
        undefined,
        url,
        { cause: error },
      )
    }
    return error
  }

  const status = `HTTP ${response.status}${response.statusText ? ` ${response.statusText}` : ''}`
  const contentType = response.headers.get('content-type') ?? ''

  if (isBlankError(error)) {
    return new ApiResponseError(`${status} from ${url} (empty response body)`, response.status, url)
  }

  if (typeof error === 'string') {
    if (isHtmlContentType(contentType) || /^\s*<(!doctype|html)/i.test(error)) {
      return new ApiResponseError(
        `${status} from ${url} (an HTML page, not a Temps API error)`,
        response.status,
        url,
      )
    }
    const excerpt = error.trim().replace(/\s+/g, ' ').slice(0, BODY_EXCERPT_CHARS)
    return new ApiResponseError(`${status} from ${url}: ${excerpt}`, response.status, url)
  }

  return error
}

export async function setupClient(
  baseUrlOverride?: string,
  apiKeyOverride?: string,
): Promise<void> {
  // An explicit override wins over config resolution. The api-key login path
  // uses this so it validates against the server the caller named, not
  // whatever context happens to be active.
  const apiUrl = normalizeApiUrl(baseUrlOverride ?? config.get('apiUrl'))

  // Pin (or clear) the key the interceptor must send. Passing `undefined`
  // resets to the normal priority-ordered resolution.
  pinnedApiKey = apiKeyOverride

  client.setConfig({
    baseUrl: apiUrl,
  })

  // Register the auth header interceptor exactly once. Re-registering on every
  // `setupClient` call (e.g. login then a follow-up request) would stack
  // duplicate interceptors that each re-read and re-set the header.
  if (!interceptorRegistered) {
    interceptorRegistered = true
    client.interceptors.request.use(async (request: Request) => {
      // A pinned key (from an explicit `--api-key`) is authoritative — it must
      // not be shadowed by an active context or env var during login.
      const apiKey = pinnedApiKey ?? (await credentials.getApiKey())
      if (apiKey) {
        request.headers.set('Authorization', `Bearer ${apiKey}`)
      }
      return request
    })
    client.interceptors.response.use(assertTempsApiResponse)
    client.interceptors.error.use(describeApiFailure)
  }
}

/**
 * Get the web dashboard base URL (API URL without /api suffix)
 */
export function getWebUrl(): string {
  return config.get('apiUrl').replace(/\/+$/, '').replace(/\/api$/, '')
}

/**
 * Extract error message from API error response
 */
export function getErrorMessage(error: unknown): string {
  if (!error) return 'Unknown error'

  // Handle object with message property
  if (typeof error === 'object' && error !== null) {
    if ('message' in error && typeof error.message === 'string') {
      return error.message
    }
    if ('detail' in error && typeof error.detail === 'string') {
      return error.detail
    }
    if ('error' in error && typeof error.error === 'string') {
      return error.error
    }
    // Try to stringify the error object
    try {
      const text = JSON.stringify(error)
      if (text === '{}') return 'The server returned an error with no details'
      return text
    } catch {
      return String(error)
    }
  }

  return String(error)
}

export { client }
