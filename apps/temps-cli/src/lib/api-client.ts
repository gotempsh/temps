import { client } from '../api/client.gen.js'
import { config, credentials } from '../config/store.js'

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
      return JSON.stringify(error)
    } catch {
      return String(error)
    }
  }

  return String(error)
}

export { client }
