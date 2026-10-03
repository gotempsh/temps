// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import {
  debugEnabled,
  serverBaseUrl,
  absoluteVerificationUri,
  pollForDeviceApproval,
  retryAfterMs,
  type DeviceStartResponse,
} from './login.js'

describe('serverBaseUrl', () => {
  test('adds /api to a bare host', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es')).toBe('https://app.temps.kfs.es/api')
  })

  test('leaves an already-/api-suffixed URL as-is', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es/api')).toBe('https://app.temps.kfs.es/api')
  })

  test('strips a trailing slash before adding /api', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es/')).toBe('https://app.temps.kfs.es/api')
  })

  test('strips multiple trailing slashes', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es///')).toBe('https://app.temps.kfs.es/api')
  })
})

describe('debugEnabled', () => {
  const originalEnv = process.env.TEMPS_DEBUG

  beforeEach(() => {
    delete process.env.TEMPS_DEBUG
  })

  afterEach(() => {
    if (originalEnv === undefined) delete process.env.TEMPS_DEBUG
    else process.env.TEMPS_DEBUG = originalEnv
  })

  test('true when --debug is passed', () => {
    expect(debugEnabled({ debug: true })).toBe(true)
  })

  test('false with no flag and no env var', () => {
    expect(debugEnabled({})).toBe(false)
    expect(debugEnabled()).toBe(false)
  })

  test.each(['1', 'true', 'yes'])('TEMPS_DEBUG=%s enables debug logging', (value) => {
    process.env.TEMPS_DEBUG = value
    expect(debugEnabled({})).toBe(true)
  })

  test('an unrecognized TEMPS_DEBUG value does not enable debug logging', () => {
    process.env.TEMPS_DEBUG = 'verbose'
    expect(debugEnabled({})).toBe(false)
  })
})

describe('absoluteVerificationUri', () => {
  const start: DeviceStartResponse = {
    device_code: 'dc_123',
    user_code: 'ABCD-1234',
    verification_uri: '/cli-login',
    verification_uri_complete: '/cli-login/ABCD-1234',
    expires_in: 900,
    interval: 2,
  }

  test('returns an already-absolute URI unchanged', () => {
    const absolute = { ...start, verification_uri_complete: 'https://app.temps.kfs.es/cli-login/CODE' }
    expect(absoluteVerificationUri('https://app.temps.kfs.es', absolute)).toBe(
      'https://app.temps.kfs.es/cli-login/CODE',
    )
  })

  test('resolves a path starting with / against the base URL', () => {
    expect(absoluteVerificationUri('https://app.temps.kfs.es', start)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })

  test('adds the missing leading slash for a bare relative path', () => {
    const relative = { ...start, verification_uri_complete: 'cli-login/ABCD-1234' }
    expect(absoluteVerificationUri('https://app.temps.kfs.es', relative)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })

  test('strips a trailing slash on the base URL before joining', () => {
    expect(absoluteVerificationUri('https://app.temps.kfs.es/', start)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })
})

describe('pollForDeviceApproval', () => {
  const approved = {
    status: 'approved',
    user_id: 1,
    email: 'dev@example.com',
    role: 'admin',
    api_key: 'tk_test',
    key_prefix: 'tk_te',
    expires_at: null,
  }

  type Reply = { status: number; body?: unknown; headers?: Record<string, string> }

  /** Replays `replies` in order against a fake clock; records every sleep. */
  function harness(replies: Reply[], opts: { intervalSecs?: number; expiresInSecs?: number } = {}) {
    let clock = 0
    const sleeps: number[] = []
    let polls = 0
    const run = () =>
      pollForDeviceApproval({
        pollUrl: 'https://temps.example/api/auth/cli/device/poll',
        intervalSecs: opts.intervalSecs ?? 2,
        expiresInSecs: opts.expiresInSecs ?? 900,
        now: () => clock,
        sleep: async (ms) => {
          sleeps.push(ms)
          clock += ms
        },
        poll: async () => {
          const reply = replies[Math.min(polls, replies.length - 1)] as Reply
          polls++
          const rawBody = reply.body === undefined ? '' : JSON.stringify(reply.body)
          const res = new Response(rawBody || null, {
            status: reply.status,
            headers: reply.headers,
          })
          return { res, rawBody, json: reply.body ?? null }
        },
      })
    return { run, sleeps, polls: () => polls }
  }

  test('keeps the advertised interval while pending', async () => {
    const h = harness([
      { status: 200, body: { status: 'authorization_pending' } },
      { status: 200, body: { status: 'authorization_pending' } },
      { status: 200, body: approved },
    ])
    expect((await h.run()).api_key).toBe('tk_test')
    expect(h.sleeps).toEqual([2000, 2000, 2000])
  })

  test('slow_down adds 5 seconds for every later poll (RFC 8628 3.5)', async () => {
    const h = harness([
      { status: 200, body: { status: 'slow_down' } },
      { status: 200, body: { status: 'authorization_pending' } },
      { status: 200, body: { status: 'slow_down' } },
      { status: 200, body: approved },
    ])
    await h.run()
    expect(h.sleeps).toEqual([2000, 7000, 7000, 12000])
  })

  test('HTTP 429 backs off instead of aborting the login', async () => {
    const h = harness([
      { status: 429, body: undefined },
      { status: 429, body: undefined },
      { status: 200, body: approved },
    ])
    expect((await h.run()).api_key).toBe('tk_test')
    expect(h.sleeps).toEqual([2000, 7000, 12000])
  })

  test('honours Retry-After in seconds on a 429', async () => {
    const h = harness([
      { status: 429, headers: { 'Retry-After': '30' } },
      { status: 200, body: approved },
    ])
    await h.run()
    expect(h.sleeps).toEqual([2000, 30000])
  })

  test('caps the back-off at 60 seconds', async () => {
    const h = harness([
      { status: 429, headers: { 'Retry-After': '3600' } },
      { status: 200, body: approved },
    ])
    await h.run()
    expect(h.sleeps).toEqual([2000, 60000])
  })

  test('never sleeps past the session lifetime, and says why it timed out', async () => {
    const h = harness([{ status: 429, headers: { 'Retry-After': '50' } }], { expiresInSecs: 30 })
    await expect(h.run()).rejects.toThrow(/HTTP 429/)
    expect(h.sleeps.reduce((a, b) => a + b, 0)).toBe(30000)
  })

  test('plain timeout keeps the plain message', async () => {
    const h = harness([{ status: 200, body: { status: 'authorization_pending' } }], {
      expiresInSecs: 5,
    })
    await expect(h.run()).rejects.toThrow('Timed out waiting for browser approval. Run `temps login` again.')
  })

  test('other error statuses still fail fast with the problem detail', async () => {
    const h = harness([{ status: 500, body: { title: 'Internal', detail: 'database unavailable' } }])
    await expect(h.run()).rejects.toThrow('database unavailable')
    expect(h.polls()).toBe(1)
  })

  test('access_denied and expired_token are terminal', async () => {
    await expect(harness([{ status: 200, body: { status: 'access_denied' } }]).run()).rejects.toThrow(
      /denied/,
    )
    await expect(harness([{ status: 200, body: { status: 'expired_token' } }]).run()).rejects.toThrow(
      /expired/,
    )
  })
})

describe('retryAfterMs', () => {
  test('parses delay-seconds', () => {
    expect(retryAfterMs('60', 0)).toBe(60000)
  })

  test('parses an HTTP-date relative to now', () => {
    const now = Date.parse('2026-01-01T00:00:00Z')
    expect(retryAfterMs('Thu, 01 Jan 2026 00:00:10 GMT', now)).toBe(10000)
  })

  test('returns null for a missing or garbage header', () => {
    expect(retryAfterMs(null, 0)).toBeNull()
    expect(retryAfterMs('soon', 0)).toBeNull()
  })
})
