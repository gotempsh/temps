// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { httpStatusOf } from './http-error-status'
import { isVerifiedNotFound } from './read-failure'
import { shouldRetryQuery } from './query-retry'
import { listScheduleRunJobs } from './schedule-runs'

const realFetch = globalThis.fetch

function respondWith(status: number, body: unknown) {
  globalThis.fetch = (async () =>
    new Response(JSON.stringify(body), {
      status,
      headers: { 'content-type': 'application/problem+json' },
    })) as unknown as typeof fetch
}

afterEach(() => {
  globalThis.fetch = realFetch
})

describe('run history reads keep the HTTP status', () => {
  test('a missing run is a verified 404 the retry policy does not repeat', async () => {
    respondWith(404, {
      title: 'Resource Not Found',
      detail: 'Schedule run 9 not found',
    })
    const error = await listScheduleRunJobs(9).catch((e: unknown) => e)
    expect(error).toBeInstanceOf(Error)
    expect((error as Error).message).toBe('Schedule run 9 not found')
    expect(httpStatusOf(error)).toBe(404)
    expect(isVerifiedNotFound(error)).toBe(true)
    expect(shouldRetryQuery(0, error)).toBe(false)
  })

  test('a server failure stays retryable and is not a missing run', async () => {
    respondWith(500, { title: 'Internal Server Error', detail: 'db down' })
    const error = await listScheduleRunJobs(9).catch((e: unknown) => e)
    expect(httpStatusOf(error)).toBe(500)
    expect(isVerifiedNotFound(error)).toBe(false)
    expect(shouldRetryQuery(0, error)).toBe(true)
  })
})
