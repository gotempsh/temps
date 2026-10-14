// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MAX_QUERY_RETRIES,
  refetchUnlessRefused,
  shouldRetryQuery,
} from './query-retry'

describe('shouldRetryQuery', () => {
  test('never retries a deterministic 4xx answer', () => {
    for (const status of [400, 401, 403, 404, 409, 410, 422]) {
      expect(shouldRetryQuery(0, { title: 'x', status })).toBe(false)
    }
  })

  test('retries a missing last deployment zero times', () => {
    // GET /projects/{id}/last-deployment answers 404 only for an unknown project.
    expect(
      shouldRetryQuery(0, {
        title: 'Not Found',
        detail: 'No deployments',
        status: 404,
      })
    ).toBe(false)
  })

  test('keeps retrying 5xx, timeouts and rate limits up to the limit', () => {
    for (const status of [408, 429, 500, 502, 503, 504]) {
      expect(shouldRetryQuery(0, { title: 'x', status })).toBe(true)
      expect(
        shouldRetryQuery(MAX_QUERY_RETRIES - 1, { title: 'x', status })
      ).toBe(true)
      expect(shouldRetryQuery(MAX_QUERY_RETRIES, { title: 'x', status })).toBe(
        false
      )
    }
  })

  test('keeps retrying network failures and errors without a status', () => {
    expect(shouldRetryQuery(0, new TypeError('Failed to fetch'))).toBe(true)
    expect(shouldRetryQuery(1, 'Bad Gateway')).toBe(true)
    expect(shouldRetryQuery(MAX_QUERY_RETRIES, new Error('boom'))).toBe(false)
  })
})

describe('refetchUnlessRefused', () => {
  const stateOf = (error: unknown) => ({ state: { error } })

  test('keeps polling a healthy or transiently failing read', () => {
    const interval = refetchUnlessRefused(30_000)
    expect(interval(stateOf(null))).toBe(30_000)
    expect(interval(stateOf({ title: 'x', status: 503 }))).toBe(30_000)
    expect(interval(stateOf(new TypeError('Failed to fetch')))).toBe(30_000)
  })

  test('stops polling once the server has refused the read', () => {
    const interval = refetchUnlessRefused(30_000)
    expect(interval(stateOf({ title: 'Forbidden', status: 403 }))).toBe(false)
    expect(interval(stateOf({ title: 'Unauthorized', status: 401 }))).toBe(
      false
    )
  })
})
