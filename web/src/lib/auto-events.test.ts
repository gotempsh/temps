// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { TrafficAggregationResponse } from '@/api/client/types.gen'
import {
  MAX_NOT_FOUND_WINDOW_DAYS,
  compactUrl,
  mergeNotFoundRows,
  notFoundPathsRequest,
  notFoundReferrersRequest,
  notFoundWindow,
  propertyValueLabel,
} from './auto-events'

const end = new Date('2026-11-19T12:00:00Z')

function response(
  rows: Array<{
    path: string
    referrer?: string | null
    requests: number
    last_seen?: string
  }>,
  withReferrer: boolean
): TrafficAggregationResponse {
  return {
    dimensions: ['path'],
    metrics: ['requests'],
    page: 1,
    page_size: 100,
    synthetic_excluded: true,
    total_groups: rows.length,
    total_pages: 1,
    rows: rows.map((row) => ({
      dimensions: [
        { dimension: 'path', value: row.path },
        // The generated union does not know `referrer` yet.
        ...(withReferrer
          ? [{ dimension: 'referrer' as never, value: row.referrer ?? null }]
          : []),
      ],
      metrics: { requests: row.requests, last_seen: row.last_seen ?? null },
    })),
  }
}

describe('notFoundWindow', () => {
  test('keeps a selection within the aggregation limit', () => {
    const start = new Date(end.getTime() - 7 * 86_400_000)
    expect(notFoundWindow(start, end)).toEqual({ start, end, narrowed: false })
  })

  test('narrows a longer selection to its most recent 30 days', () => {
    const start = new Date(end.getTime() - 90 * 86_400_000)
    const window = notFoundWindow(start, end)
    expect(window.narrowed).toBe(true)
    expect(end.getTime() - window.start.getTime()).toBe(
      MAX_NOT_FOUND_WINDOW_DAYS * 86_400_000
    )
  })
})

describe('not-found requests', () => {
  const window = notFoundWindow(new Date('2026-11-12T12:00:00Z'), end)

  test('ranks 404 paths answered to real visitors', () => {
    const request = notFoundPathsRequest(window, 4)
    expect(request.dimensions).toEqual(['path'])
    expect(request.environment_id).toBe(4)
    expect(request.filters).toEqual([
      { dimension: 'status_code', operator: 'eq', values: ['404'] },
      { dimension: 'is_bot', operator: 'eq', values: ['false'] },
    ])
    expect(request.order_by).toEqual([
      { field: { kind: 'metric', field: 'requests' }, direction: 'desc' },
    ])
  })

  test('asks for referrers of the listed paths only, without empty referrers', () => {
    const request = notFoundReferrersRequest(window, undefined, ['/a', '/b'])
    expect(request?.dimensions).toEqual(['path', 'referrer'])
    expect(request?.environment_id).toBeNull()
    expect(request?.filters).toContainEqual({
      dimension: 'path',
      operator: 'in',
      values: ['/a', '/b'],
    })
    expect(request?.filters).toContainEqual({
      dimension: 'referrer',
      operator: 'not_eq',
      values: [''],
    })
  })

  test('skips the referrer request when there are no paths', () => {
    expect(notFoundReferrersRequest(window, undefined, [])).toBeNull()
  })

  test('never sends more than the 20 values an in filter accepts', () => {
    const paths = Array.from({ length: 30 }, (_, i) => `/p${i}`)
    const request = notFoundReferrersRequest(window, undefined, paths)
    const inFilter = request?.filters.find((filter) => filter.operator === 'in')
    expect(inFilter?.values).toHaveLength(20)
  })
})

describe('mergeNotFoundRows', () => {
  const paths = response(
    [
      { path: '/old-pricing', requests: 40, last_seen: '2026-11-19T10:00:00Z' },
      { path: '/typo', requests: 3 },
    ],
    false
  )

  test('attaches the most frequent referrer to each path', () => {
    const referrers = response(
      [
        {
          path: '/old-pricing',
          referrer: 'https://blog.example.org/post',
          requests: 30,
        },
        {
          path: '/old-pricing',
          referrer: 'https://news.example.net/',
          requests: 10,
        },
      ],
      true
    )
    expect(mergeNotFoundRows(paths, referrers)).toEqual([
      {
        path: '/old-pricing',
        hits: 40,
        lastSeen: '2026-11-19T10:00:00Z',
        topReferrer: 'https://blog.example.org/post',
      },
      { path: '/typo', hits: 3, lastSeen: null, topReferrer: null },
    ])
  })

  test('still lists paths when the referrer query was unavailable', () => {
    expect(
      mergeNotFoundRows(paths, null).map((row) => row.topReferrer)
    ).toEqual([null, null])
  })
})

describe('labels', () => {
  test('names a missing property value', () => {
    expect(propertyValueLabel(null)).toBe('(not set)')
    expect(propertyValueLabel('')).toBe('(not set)')
    expect(propertyValueLabel('pdf')).toBe('pdf')
  })

  test('shortens URLs for display', () => {
    expect(compactUrl('https://partner.example.org/pricing')).toBe(
      'partner.example.org/pricing'
    )
    expect(compactUrl('http://example.org/')).toBe('example.org')
  })
})
