// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { TrafficAggregationRequest } from '@/api/client/types.gen'
import { parseStructuredSseFrame } from '@/hooks/useStructuredAi'
import {
  apiTrafficSummaryCacheKey,
  canStartApiTrafficSummary,
  shouldRequestApiTrafficSummary,
} from '@/lib/ai-summary'
import {
  nextTrafficSort,
  trafficPageCount,
  trafficQueryNeedsCompatibilityRetry,
} from '@/lib/api-traffic-sort'

describe('API traffic server-side sorting', () => {
  test('toggles an active metric from descending to ascending', () => {
    expect(
      nextTrafficSort({ metric: 'error_rate', direction: 'desc' }, 'error_rate')
    ).toEqual({ metric: 'error_rate', direction: 'asc' })
  })

  test('starts a newly selected metric in descending order', () => {
    expect(
      nextTrafficSort({ metric: 'requests', direction: 'asc' }, 'latency_avg')
    ).toEqual({ metric: 'latency_avg', direction: 'desc' })
  })

  test('paginates high-cardinality route and caller drilldowns', () => {
    expect(trafficPageCount(100_000, 20)).toBe(5_000)
    expect(trafficPageCount(1_000, 20)).toBe(50)
  })
})

describe('API traffic rolling-upgrade compatibility', () => {
  const request: TrafficAggregationRequest = {
    start_time: '2026-08-15T00:00:00Z',
    end_time: '2026-08-16T00:00:00Z',
    dimensions: ['path'],
    metrics: ['requests', 'bot_requests'],
  }

  test('retries when an older backend rejects an optional crawler metric', () => {
    expect(
      trafficQueryNeedsCompatibilityRetry(request, {
        detail:
          'metrics[1]: unknown variant `bot_requests`, expected one of `requests`, `errors`',
      })
    ).toBe(true)
  })

  test('does not hide unrelated aggregation errors', () => {
    expect(
      trafficQueryNeedsCompatibilityRetry(request, {
        detail: 'database query timed out',
      })
    ).toBe(false)
  })
})

describe('structured AI SSE parsing', () => {
  test('parses a partial JSON event', () => {
    expect(
      parseStructuredSseFrame(
        'event: partial\r\ndata: {"type":"partial","value":{"headline":"Ready"}}'
      )
    ).toEqual({ type: 'partial', value: { headline: 'Ready' } })
  })

  test('ignores keep-alive frames without data', () => {
    expect(parseStructuredSseFrame(': keep-alive')).toBeNull()
  })

  test('parses resolved provider, model, and thinking metadata', () => {
    expect(
      parseStructuredSseFrame(
        'event: started\ndata: {"type":"started","cached":false,"provider":"openai","model":"gpt-5.6","thinking_level":"high"}'
      )
    ).toEqual({
      type: 'started',
      cached: false,
      provider: 'openai',
      model: 'gpt-5.6',
      thinking_level: 'high',
    })
  })
})

describe('API traffic summary cache identity', () => {
  test('keeps rolling filters stable when their timestamps move', () => {
    const first = apiTrafficSummaryCacheKey({
      filter: '24hours',
      startIso: '2026-08-13T15:00:00Z',
      endIso: '2026-08-14T15:00:00Z',
    })
    const later = apiTrafficSummaryCacheKey({
      filter: '24hours',
      startIso: '2026-08-13T15:00:40Z',
      endIso: '2026-08-14T15:00:40Z',
    })

    expect(later).toBe(first)
  })

  test('keeps custom windows distinct', () => {
    const first = apiTrafficSummaryCacheKey({
      filter: 'custom',
      startIso: '2026-08-13T15:00:00Z',
      endIso: '2026-08-14T15:00:00Z',
    })
    const later = apiTrafficSummaryCacheKey({
      filter: 'custom',
      startIso: '2026-08-13T15:00:40Z',
      endIso: '2026-08-14T15:00:40Z',
    })

    expect(later).not.toBe(first)
  })
})

describe('API traffic summary request gating', () => {
  test('lets the user start an on-demand summary before context is loaded', () => {
    expect(
      canStartApiTrafficSummary({
        analyticsEnabled: true,
        timeseriesPending: false,
        timeseriesError: false,
      })
    ).toBe(true)
  })

  test('waits for the base timeseries before starting context queries', () => {
    expect(
      canStartApiTrafficSummary({
        analyticsEnabled: true,
        timeseriesPending: true,
        timeseriesError: false,
      })
    ).toBe(false)
  })

  test('does not start when the required base analytics failed', () => {
    expect(
      canStartApiTrafficSummary({
        analyticsEnabled: true,
        timeseriesPending: false,
        timeseriesError: true,
      })
    ).toBe(false)
  })

  test('never spends AI credits before an explicit request', () => {
    expect(
      shouldRequestApiTrafficSummary({
        requested: false,
        dataReady: true,
        consentEnabled: true,
      })
    ).toBe(false)
  })

  test('requires consent and loaded analytics after the user requests it', () => {
    expect(
      shouldRequestApiTrafficSummary({
        requested: true,
        dataReady: false,
        consentEnabled: true,
      })
    ).toBe(false)
    expect(
      shouldRequestApiTrafficSummary({
        requested: true,
        dataReady: true,
        consentEnabled: false,
      })
    ).toBe(false)
    expect(
      shouldRequestApiTrafficSummary({
        requested: true,
        dataReady: true,
        consentEnabled: true,
      })
    ).toBe(true)
  })
})
