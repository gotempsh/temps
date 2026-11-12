// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { formatUpdatedAgo, keepPreviousWindow, queryKeyPrefix } from './refresh'

describe('formatUpdatedAgo', () => {
  const at = Date.UTC(2026, 0, 1)
  test.each([
    [0, 'Updated just now'],
    [59_000, 'Updated just now'],
    [2 * 60_000, 'Updated 2m ago'],
    [3 * 3_600_000, 'Updated 3h ago'],
    [2 * 86_400_000, 'Updated 2d ago'],
    [-5_000, 'Updated just now'],
  ])('%i ms later reads %s', (elapsed, text) => {
    expect(formatUpdatedAgo(at, at + elapsed)).toBe(text)
  })
})

describe('queryKeyPrefix', () => {
  const listLogsQueryKey = (options: {
    path: { project_id: number }
    query?: { page?: number; status?: string }
  }) => [{ _id: 'listLogs', baseUrl: '', ...options }] as const

  test('builds the key from the scope it is given and nothing else', () => {
    expect(queryKeyPrefix(listLogsQueryKey)).toEqual([
      { _id: 'listLogs', baseUrl: '' },
    ])
    expect(
      queryKeyPrefix(listLogsQueryKey, { path: { project_id: 7 } })
    ).toEqual([{ _id: 'listLogs', baseUrl: '', path: { project_id: 7 } }])
  })
})

describe('keepPreviousWindow', () => {
  const key = (query: Record<string, unknown>) =>
    [
      { _id: 'getBreakdown', baseUrl: '', path: { project_id: 7 }, query },
    ] as const
  const window = {
    start_date: '2026-01-01T00:00:00Z',
    end_date: '2026-01-02T00:00:00Z',
  }
  const moved = {
    start_date: '2026-01-01T00:05:00Z',
    end_date: '2026-01-02T00:05:00Z',
  }
  const rows = ['France', 'Germany']

  test('keeps the previous data when only the window moved', () => {
    const placeholder = keepPreviousWindow(
      key({ ...moved, group_by: 'country' })
    )
    expect(
      placeholder(rows, { queryKey: key({ ...window, group_by: 'country' }) })
    ).toBe(rows)
  })

  test('drops it when anything else changed: a drill-down, a service, a page', () => {
    const drilled = keepPreviousWindow(
      key({ ...window, group_by: 'region', filter_country: 'France' })
    )
    expect(
      drilled(rows, { queryKey: key({ ...window, group_by: 'country' }) })
    ).toBeUndefined()
    const otherService = keepPreviousWindow([
      {
        _id: 'getBreakdown',
        baseUrl: '',
        path: { project_id: 8 },
        query: window,
      },
    ])
    expect(otherService(rows, { queryKey: key(window) })).toBeUndefined()
  })

  test('keeps it when a range anchored to a day only moves its end ("today")', () => {
    const today = keepPreviousWindow(
      key({ start_date: window.start_date, end_date: moved.end_date })
    )
    expect(today(rows, { queryKey: key(window) })).toBe(rows)
  })

  test('drops it when another range is picked, wider, narrower or elsewhere', () => {
    const lastWeek = keepPreviousWindow(
      key({ start_date: '2025-12-26T00:05:00Z', end_date: moved.end_date })
    )
    expect(lastWeek(rows, { queryKey: key(window) })).toBeUndefined()
    const lastHour = keepPreviousWindow(
      key({ start_date: '2026-01-01T23:05:00Z', end_date: moved.end_date })
    )
    expect(lastHour(rows, { queryKey: key(window) })).toBeUndefined()
    const nextDay = keepPreviousWindow(
      key({ start_date: window.end_date, end_date: '2026-01-03T00:00:00Z' })
    )
    expect(nextDay(rows, { queryKey: key(window) })).toBeUndefined()
    const earlier = keepPreviousWindow(
      key({
        start_date: '2025-12-31T00:00:00Z',
        end_date: '2026-01-01T00:00:00Z',
      })
    )
    expect(earlier(rows, { queryKey: key(window) })).toBeUndefined()
  })

  test('reads a window named in a hand-written key', () => {
    const usageKey = (from: string, to: string) => [
      'aiUsage',
      'summary',
      { from, to },
    ]
    const refreshed = keepPreviousWindow(
      usageKey(moved.start_date, moved.end_date)
    )
    expect(
      refreshed(rows, {
        queryKey: usageKey(window.start_date, window.end_date),
      })
    ).toBe(rows)
    const wider = keepPreviousWindow(
      usageKey('2025-12-02T00:05:00Z', moved.end_date)
    )
    expect(
      wider(rows, { queryKey: usageKey(window.start_date, window.end_date) })
    ).toBeUndefined()
  })

  test('has nothing to keep without a previous query', () => {
    expect(keepPreviousWindow(key(window))(rows, undefined)).toBeUndefined()
  })
})
