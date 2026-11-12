// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  QueryClient,
  type QueryCache,
  type QueryKey,
} from '@tanstack/react-query'
import {
  forgetWindowMove,
  formatUpdatedAgo,
  isMovingWindows,
  keepPreviousWindow,
  moveWindows,
  previousWindowQuery,
  queryKeyPrefix,
  recordWindowMove,
} from './refresh'

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

  /** The cache after `from` loaded `rows`. */
  function loaded(from: QueryKey): QueryCache {
    const client = new QueryClient()
    client.setQueryData(from, rows)
    return client.getQueryCache()
  }

  /** What `to` shows while it loads, after `from`'s window moved to it. */
  function afterMove(from: QueryKey, to: QueryKey, cache = loaded(from)) {
    recordWindowMove(cache, from, to)
    return keepPreviousWindow(to)(rows, { queryKey: from })
  }

  test('keeps the previous data when only the window moved', () => {
    expect(
      afterMove(
        key({ ...window, group_by: 'country' }),
        key({ ...moved, group_by: 'country' })
      )
    ).toBe(rows)
  })

  test('keeps it for a move of any shape: an end-only move, a short window long idle', () => {
    expect(
      afterMove(
        key(window),
        key({ start_date: window.start_date, end_date: moved.end_date })
      )
    ).toBe(rows)
    expect(
      afterMove(
        key({
          start_date: '2026-01-02T00:00:00Z',
          end_date: '2026-01-02T01:00:00Z',
        }),
        key({
          start_date: '2026-01-02T05:00:00Z',
          end_date: '2026-01-02T06:00:00Z',
        })
      )
    ).toBe(rows)
  })

  test('drops it when a move changed anything else: a drill-down, a service', () => {
    expect(
      afterMove(
        key({ ...window, group_by: 'country' }),
        key({ ...moved, group_by: 'region', filter_country: 'France' })
      )
    ).toBeUndefined()
    expect(
      afterMove(key(window), [
        {
          _id: 'getBreakdown',
          baseUrl: '',
          path: { project_id: 8 },
          query: moved,
        },
      ])
    ).toBeUndefined()
  })

  test('drops it for a range picked rather than moved, however close to now it ends', () => {
    // An hour picked as a custom range ending now looks exactly like "last
    // hour" moved by Refresh; only a recorded move keeps the earlier data.
    const earlierHour = key({
      start_date: '2026-01-02T10:00:00Z',
      end_date: '2026-01-02T11:00:00Z',
    })
    const pickedHour = key({
      start_date: '2026-01-02T12:00:00Z',
      end_date: '2026-01-02T13:00:00Z',
    })
    expect(
      keepPreviousWindow(pickedHour)(rows, { queryKey: earlierHour })
    ).toBeUndefined()
  })

  test('a key picked again after a move is a new selection', () => {
    const from = key({ ...window, group_by: 'browser' })
    const to = key({ ...moved, group_by: 'browser' })
    expect(afterMove(from, to)).toBe(rows)
    forgetWindowMove(to)
    expect(keepPreviousWindow(to)(rows, { queryKey: from })).toBeUndefined()
  })

  test('a window moved again before it loaded keeps the data it was keeping', () => {
    const first = key({ ...window, group_by: 'device' })
    const second = key({ ...moved, group_by: 'device' })
    const third = key({
      start_date: '2026-01-01T00:10:00Z',
      end_date: '2026-01-02T00:10:00Z',
      group_by: 'device',
    })
    const cache = loaded(first)
    recordWindowMove(cache, first, second)
    recordWindowMove(cache, second, third)
    expect(keepPreviousWindow(third)(rows, { queryKey: first })).toBe(rows)
    expect(previousWindowQuery(cache, third)?.queryKey).toEqual(first)
  })

  test('a window moved from one that never loaded keeps nothing', () => {
    const from = key({ ...window, group_by: 'os' })
    const to = key({ ...moved, group_by: 'os' })
    const cache = new QueryClient().getQueryCache()
    recordWindowMove(cache, from, to)
    expect(previousWindowQuery(cache, to)).toBeUndefined()
    expect(keepPreviousWindow(to)(rows, { queryKey: from })).toBeUndefined()
  })

  test('reads a window named in a hand-written key', () => {
    const usageKey = (from: string, to: string) => [
      'aiUsage',
      'summary',
      { from, to },
    ]
    expect(
      afterMove(
        usageKey(window.start_date, window.end_date),
        usageKey(moved.start_date, moved.end_date)
      )
    ).toBe(rows)
  })

  test('has nothing to keep without a previous query', () => {
    expect(keepPreviousWindow(key(window))(rows, undefined)).toBeUndefined()
  })
})

describe('moveWindows', () => {
  test('marks the key changes it renders as moves, and only those', () => {
    expect(isMovingWindows()).toBe(false)
    let during = false
    moveWindows(() => {
      during = isMovingWindows()
    })
    expect(during).toBe(true)
    expect(isMovingWindows()).toBe(false)
  })

  test('ends the move even when the update throws', () => {
    expect(() =>
      moveWindows(() => {
        throw new Error('update failed')
      })
    ).toThrow('update failed')
    expect(isMovingWindows()).toBe(false)
  })
})
