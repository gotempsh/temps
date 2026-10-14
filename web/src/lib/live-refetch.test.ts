// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  ACTIVE_LIST_POLL_MS,
  FOCUS_REFETCH_MIN_AGE_MS,
  IDLE_LIST_POLL_MS,
  UNBOUNDED_LIST_IDLE_POLL_MS,
  UNBOUNDED_LIST_MAX_IDLE_POLLED_ROWS,
  liveListInterval,
  refetchOnFocusWhenStale,
  sourceBackupsRefetchInterval,
} from './live-refetch'

describe('liveListInterval', () => {
  test('polls fast while rows are in progress', () => {
    expect(liveListInterval(true)).toBe(ACTIVE_LIST_POLL_MS)
  })

  test('keeps a slower cadence when idle so background changes still appear', () => {
    expect(liveListInterval(false)).toBe(IDLE_LIST_POLL_MS)
    expect(IDLE_LIST_POLL_MS).toBeGreaterThan(ACTIVE_LIST_POLL_MS)
  })
})

describe('sourceBackupsRefetchInterval', () => {
  test('polls fast while a backup is pending or running', () => {
    expect(
      sourceBackupsRefetchInterval([
        { state: 'completed' },
        { state: 'running' },
      ])
    ).toBe(ACTIVE_LIST_POLL_MS)
    expect(sourceBackupsRefetchInterval([{ state: 'pending' }])).toBe(
      ACTIVE_LIST_POLL_MS
    )
  })

  test('keeps the slower idle cadence once every backup is final or none exist', () => {
    expect(
      sourceBackupsRefetchInterval([
        { state: 'completed' },
        { state: 'failed' },
      ])
    ).toBe(UNBOUNDED_LIST_IDLE_POLL_MS)
    expect(sourceBackupsRefetchInterval([])).toBe(UNBOUNDED_LIST_IDLE_POLL_MS)
    expect(sourceBackupsRefetchInterval(undefined)).toBe(
      UNBOUNDED_LIST_IDLE_POLL_MS
    )
    expect(UNBOUNDED_LIST_IDLE_POLL_MS).toBeGreaterThan(IDLE_LIST_POLL_MS)
  })

  test('stops idle polling a heavy history but still follows running work', () => {
    const heavy = Array.from(
      { length: UNBOUNDED_LIST_MAX_IDLE_POLLED_ROWS + 1 },
      () => ({ state: 'completed' })
    )
    expect(sourceBackupsRefetchInterval(heavy)).toBe(false)
    expect(sourceBackupsRefetchInterval([...heavy, { state: 'running' }])).toBe(
      ACTIVE_LIST_POLL_MS
    )
  })
})

describe('refetchOnFocusWhenStale', () => {
  const now = 1_000_000

  test('refetches data older than the minimum age', () => {
    const query = { state: { dataUpdatedAt: now - FOCUS_REFETCH_MIN_AGE_MS } }
    expect(refetchOnFocusWhenStale(query, now)).toBe(true)
  })

  test('skips data fetched moments ago', () => {
    const query = { state: { dataUpdatedAt: now - 1_000 } }
    expect(refetchOnFocusWhenStale(query, now)).toBe(false)
  })

  test('leaves a query that never fetched to its own mount fetch', () => {
    expect(refetchOnFocusWhenStale({ state: { dataUpdatedAt: 0 } }, now)).toBe(
      false
    )
  })
})
