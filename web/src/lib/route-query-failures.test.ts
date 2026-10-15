// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import {
  ROUTE_FAILURE_IGNORE,
  collectRouteQueryFailures,
  isReportableReadFailure,
  markReadFailureHandled,
  representativeFailure,
  subscribeToHandledReadFailures,
} from './route-query-failures'

function problem(status: number, title = 'Problem') {
  return { status, title, detail: `HTTP ${status}` }
}

/** A client whose single query for `key` has failed with `error`. */
async function failedQuery(
  client: QueryClient,
  key: string,
  error: unknown,
  { observed = true }: { observed?: boolean } = {}
) {
  const observer = new QueryObserver(client, {
    queryKey: [key],
    queryFn: () => Promise.reject(error),
    retry: false,
  })
  const unsubscribe = observed ? observer.subscribe(() => {}) : () => {}
  await observer.refetch().catch(() => {})
  return unsubscribe
}

describe('isReportableReadFailure', () => {
  test('reports server failures, refusals and unreachable servers', () => {
    expect(isReportableReadFailure(problem(500))).toBe(true)
    expect(isReportableReadFailure(problem(503))).toBe(true)
    expect(isReportableReadFailure(problem(403, 'Forbidden'))).toBe(true)
    expect(isReportableReadFailure(problem(408))).toBe(true)
    expect(isReportableReadFailure(problem(429))).toBe(true)
    // A dropped connection has no HTTP status at all.
    expect(isReportableReadFailure(new TypeError('Failed to fetch'))).toBe(true)
  })

  test('leaves answers the page must interpret itself', () => {
    expect(isReportableReadFailure(problem(404, 'Not Found'))).toBe(false)
    expect(isReportableReadFailure(problem(400, 'Bad Request'))).toBe(false)
    expect(isReportableReadFailure(problem(401, 'Unauthorized'))).toBe(false)
    expect(isReportableReadFailure(problem(409, 'Conflict'))).toBe(false)
  })
})

describe('collectRouteQueryFailures', () => {
  test('finds a failed read that something on screen is waiting for', async () => {
    const client = new QueryClient()
    const stop = await failedQuery(client, 'projects', problem(500))
    expect(collectRouteQueryFailures(client)).toHaveLength(1)
    stop()
  })

  test('ignores a failed read nothing is observing any more', async () => {
    const client = new QueryClient()
    const stop = await failedQuery(client, 'left-page', problem(500))
    stop()
    expect(collectRouteQueryFailures(client)).toHaveLength(0)
  })

  test('ignores a background read that opted out of route reporting', async () => {
    const client = new QueryClient()
    const observer = new QueryObserver(client, {
      queryKey: ['optional-banner'],
      queryFn: () => Promise.reject(problem(403, 'Forbidden')),
      retry: false,
      meta: { routeFailure: ROUTE_FAILURE_IGNORE },
    })
    const stop = observer.subscribe(() => {})
    await observer.refetch().catch(() => {})
    expect(client.getQueryState(['optional-banner'])?.status).toBe('error')
    expect(collectRouteQueryFailures(client)).toHaveLength(0)
    stop()
  })

  test('ignores a failed refetch that still has data to show', async () => {
    const client = new QueryClient()
    client.setQueryData(['poll'], { ok: true })
    const observer = new QueryObserver(client, {
      queryKey: ['poll'],
      queryFn: () => Promise.reject(problem(500)),
      retry: false,
    })
    const stop = observer.subscribe(() => {})
    await observer.refetch().catch(() => {})
    expect(client.getQueryState(['poll'])?.status).toBe('error')
    expect(collectRouteQueryFailures(client)).toHaveLength(0)
    stop()
  })

  test('ignores a 404 the page turns into its own not-found state', async () => {
    const client = new QueryClient()
    const stop = await failedQuery(client, 'one', problem(404, 'Not Found'))
    expect(collectRouteQueryFailures(client)).toHaveLength(0)
    stop()
  })

  test('ignores an error a component already explains', async () => {
    const client = new QueryClient()
    const error = problem(500)
    const stop = await failedQuery(client, 'explained', error)
    expect(collectRouteQueryFailures(client)).toHaveLength(1)
    markReadFailureHandled(error)
    expect(collectRouteQueryFailures(client)).toHaveLength(0)
    stop()
  })

  test('tells listeners when an error becomes explained', () => {
    let calls = 0
    const stop = subscribeToHandledReadFailures(() => {
      calls += 1
    })
    const error = problem(500)
    markReadFailureHandled(error)
    markReadFailureHandled(error)
    stop()
    markReadFailureHandled(problem(500))
    expect(calls).toBe(1)
  })
})

describe('representativeFailure', () => {
  test('prefers a failure a retry can fix over a refusal', () => {
    const refusal = { queryHash: 'a', error: problem(403, 'Forbidden') }
    const outage = { queryHash: 'b', error: problem(500) }
    expect(representativeFailure([refusal, outage])).toBe(outage)
    expect(representativeFailure([refusal])).toBe(refusal)
    expect(representativeFailure([])).toBeUndefined()
  })
})
