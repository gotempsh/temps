/**
 * Builds a standalone @hey-api client pointed at a live Temps instance and wires
 * the bearer-token auth header. Mirrors apps/temps-cli/src/lib/api-client.ts but
 * is fully self-contained (no CLI config/context machinery) so the e2e tool can
 * target any instance from flags/env.
 *
 * The generated SDK comes from the shared, publishable @temps-sdk/api package —
 * the single source of truth, consumed here exactly as an external consumer
 * would after `npm install @temps-sdk/api`.
 */
import { createClient, type Client } from '@temps-sdk/api/client'

export interface TempsClientConfig {
  /** Base URL of the instance, e.g. http://localhost:8080 (with or without /api). */
  url: string
  /** Bearer API key (tk_...). */
  apiKey: string
}

/** Normalize a base URL to always end in exactly one `/api`. */
export function normalizeApiUrl(url: string): string {
  let normalized = url.replace(/\/+$/, '')
  if (!normalized.endsWith('/api')) {
    normalized += '/api'
  }
  return normalized
}

/**
 * Create an isolated client instance. Every generated SDK function accepts
 * `{ client }` so this can be passed per-call without touching global state.
 */
export function makeClient(cfg: TempsClientConfig): Client {
  const baseUrl = normalizeApiUrl(cfg.url)
  const client = createClient({ baseUrl })
  client.interceptors.request.use((request: Request) => {
    request.headers.set('Authorization', `Bearer ${cfg.apiKey}`)
    return request
  })
  return client
}

/**
 * Resolve instance url + key from explicit args, falling back to env.
 * TEMPS_URL / TEMPS_API_KEY are the canonical env vars (match the CLI).
 */
export function resolveConfig(opts: { url?: string; apiKey?: string }): TempsClientConfig {
  const env = (globalThis as { process?: { env: Record<string, string | undefined> } })
    .process?.env ?? {}
  const url = opts.url || env.TEMPS_URL || 'http://localhost:8080'
  const apiKey = opts.apiKey || env.TEMPS_API_KEY || ''
  if (!apiKey) {
    throw new Error(
      'No API key. Pass --api-key or set TEMPS_API_KEY (mint one with: temps api-key --database-url=... --name=e2e --user-email=... --output-format=json)',
    )
  }
  return { url, apiKey }
}

/** Unwrap a hey-api `{ data, error, response }` result, throwing a useful error. */
export function unwrap<T>(
  result: { data?: T; error?: unknown; response?: Response },
  context: string,
): T {
  if (result.error !== undefined && result.error !== null) {
    const status = result.response?.status
    const detail =
      typeof result.error === 'object' ? JSON.stringify(result.error) : String(result.error)
    throw new Error(`${context} failed${status ? ` (HTTP ${status})` : ''}: ${detail}`)
  }
  if (result.data === undefined) {
    const status = result.response?.status
    throw new Error(`${context}: empty response${status ? ` (HTTP ${status})` : ''}`)
  }
  return result.data
}
