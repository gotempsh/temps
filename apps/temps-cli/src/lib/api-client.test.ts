import { test, expect, beforeEach, afterEach } from 'bun:test'
import { client } from '../api/client.gen.js'
import { setupClient } from './api-client.js'
import { credentials } from '../config/store.js'

/**
 * Regression tests for the api-key login auth-header resolution.
 *
 * The bug: `setupClient`'s interceptor read `credentials.getApiKey()`, which
 * resolves in priority order (env var > active context > stored secrets). When
 * an active context already held a key, a freshly-supplied `--api-key` was
 * shadowed and never sent — so `temps login --api-key=<valid>` validated the
 * WRONG key against the server and reported "Invalid API key".
 *
 * The fix: `setupClient(url, key)` pins the supplied key so the interceptor
 * sends exactly that key, independent of env vars or the active context.
 */

/**
 * The hey-api client stores registered interceptors in an internal array. The
 * field name is `_fns` in source but minifiers may rename it (e.g. `fns`), so
 * we locate the first own-property that is an array of functions rather than
 * hard-coding the name.
 */
function interceptorFns(): Array<unknown> {
  const obj = client.interceptors.request as unknown as Record<string, unknown>
  for (const name of Object.getOwnPropertyNames(obj)) {
    const value = obj[name]
    if (Array.isArray(value)) {
      return value
    }
  }
  throw new Error('Could not locate interceptor function array on client.interceptors.request')
}

/** Run every registered request interceptor against a fresh Request. */
async function runRequestInterceptors(): Promise<Request> {
  const fns = interceptorFns()
  let request = new Request('http://localhost:3000/api/user/me')
  for (const fn of fns) {
    if (typeof fn === 'function') {
      request = await (fn as (r: Request, o: unknown) => Promise<Request>)(request, {})
    }
  }
  return request
}

let savedEnv: Record<string, string | undefined> = {}

beforeEach(() => {
  // Snapshot and clear env tokens so they don't perturb resolution.
  savedEnv = {
    TEMPS_TOKEN: process.env.TEMPS_TOKEN,
    TEMPS_API_TOKEN: process.env.TEMPS_API_TOKEN,
    TEMPS_API_KEY: process.env.TEMPS_API_KEY,
  }
  delete process.env.TEMPS_TOKEN
  delete process.env.TEMPS_API_TOKEN
  delete process.env.TEMPS_API_KEY
})

afterEach(async () => {
  for (const [k, v] of Object.entries(savedEnv)) {
    if (v === undefined) delete process.env[k]
    else process.env[k] = v
  }
  // Reset the pin so other suites get clean resolution.
  await setupClient('http://localhost:3000', undefined)
})

test('pinned api key overrides whatever credentials.getApiKey() would return', async () => {
  // Simulate an active context (or env) resolving to a DIFFERENT key.
  const original = credentials.getApiKey
  credentials.getApiKey = async () => 'tk_ACTIVE_CONTEXT_KEY'
  try {
    await setupClient('http://localhost:3000', 'tk_SUPPLIED_KEY')
    const request = await runRequestInterceptors()
    expect(request.headers.get('Authorization')).toBe('Bearer tk_SUPPLIED_KEY')
  } finally {
    credentials.getApiKey = original
  }
})

test('without a pin, the interceptor falls back to credentials.getApiKey()', async () => {
  const original = credentials.getApiKey
  credentials.getApiKey = async () => 'tk_RESOLVED_KEY'
  try {
    await setupClient('http://localhost:3000', undefined)
    const request = await runRequestInterceptors()
    expect(request.headers.get('Authorization')).toBe('Bearer tk_RESOLVED_KEY')
  } finally {
    credentials.getApiKey = original
  }
})

test('clearing the pin (passing undefined) stops sending the previously pinned key', async () => {
  const original = credentials.getApiKey
  credentials.getApiKey = async () => undefined
  try {
    // First pin a key, then clear it.
    await setupClient('http://localhost:3000', 'tk_TEMP_PIN')
    await setupClient('http://localhost:3000', undefined)
    const request = await runRequestInterceptors()
    // No pin, no resolved key -> no Authorization header at all.
    expect(request.headers.get('Authorization')).toBeNull()
  } finally {
    credentials.getApiKey = original
  }
})

test('repeated setupClient calls do not stack duplicate interceptors', async () => {
  const before = interceptorFns().length
  await setupClient('http://localhost:3000', 'tk_A')
  await setupClient('http://localhost:3000', 'tk_B')
  await setupClient('http://localhost:3000', undefined)
  const after = interceptorFns().length
  // The interceptor is registered at most once across calls.
  expect(after - before).toBeLessThanOrEqual(1)
})
