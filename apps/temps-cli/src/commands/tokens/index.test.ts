// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import { credentials } from '../../config/store.js'
import { computeExpiryIso, resolveProjectId } from './index.js'

describe('computeExpiryIso', () => {
  test('advances by the given number of days from the reference date', () => {
    const from = new Date('2026-01-01T00:00:00.000Z')
    expect(computeExpiryIso('7', from)).toBe('2026-01-08T00:00:00.000Z')
  })

  test('rejects non-numeric and non-positive input', () => {
    expect(computeExpiryIso('never')).toBeNull()
    expect(computeExpiryIso('0')).toBeNull()
    expect(computeExpiryIso('-1')).toBeNull()
  })
})

describe('resolveProjectId', () => {
  test('treats a numeric slug as the project ID directly, without a network call', async () => {
    // Distinct code path from the slug lookup below: parseInt succeeds and
    // the function returns before ever calling fetch.
    let fetchCalled = false
    const originalFetch = global.fetch
    global.fetch = (() => {
      fetchCalled = true
      throw new Error('should not be called')
    }) as unknown as typeof fetch
    try {
      await expect(resolveProjectId('42')).resolves.toBe(42)
      expect(fetchCalled).toBe(false)
    } finally {
      global.fetch = originalFetch
    }
  })

  describe('slug lookup', () => {
    const originalFetch = global.fetch
    const originalGetApiKey = credentials.getApiKey
    const originalApiUrl = process.env.TEMPS_API_URL

    beforeEach(() => {
      process.env.TEMPS_API_URL = 'http://localhost:9999'
      credentials.getApiKey = async () => 'tk_TEST_KEY'
    })

    afterEach(() => {
      global.fetch = originalFetch
      credentials.getApiKey = originalGetApiKey
      if (originalApiUrl === undefined) delete process.env.TEMPS_API_URL
      else process.env.TEMPS_API_URL = originalApiUrl
    })

    test('resolves a project by slug, case-insensitively', async () => {
      global.fetch = (async () =>
        new Response(
          JSON.stringify({ projects: [{ slug: 'my-app', id: 7 }] }),
          { status: 200 },
        )) as unknown as typeof fetch

      await expect(resolveProjectId('My-App')).resolves.toBe(7)
    })

    test('raises a clear error when no project matches the slug', async () => {
      global.fetch = (async () =>
        new Response(JSON.stringify({ projects: [] }), { status: 200 })) as unknown as typeof fetch

      await expect(resolveProjectId('missing-project')).rejects.toThrow(
        /Project "missing-project" not found/,
      )
    })

    test('surfaces the server error body when the lookup request fails', async () => {
      global.fetch = (async () =>
        new Response(JSON.stringify({ detail: 'Unauthorized' }), { status: 401 })) as unknown as typeof fetch

      await expect(resolveProjectId('any-slug')).rejects.toThrow(/Unauthorized/)
    })
  })
})
