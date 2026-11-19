// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { isMovingWindows } from '@/lib/refresh'
import { useAdvancingDateRange } from './useAdvancingDateRange'

installBrowserWindow()
afterEach(cleanup)

test('a relative range moves forward to now while the page stays open', async () => {
  const { result } = renderHook(() =>
    useAdvancingDateRange({ quickFilter: '24hours', dateRange: undefined }, 20)
  )
  const first = result.current.endDate!.getTime()
  await waitFor(() =>
    expect(result.current.endDate!.getTime()).toBeGreaterThan(first)
  )
})

test('moving forward on its own is a window move, not a new selection', async () => {
  const moves: boolean[] = []
  const { result } = renderHook(() => {
    const range = useAdvancingDateRange(
      { quickFilter: '24hours', dateRange: undefined },
      20
    )
    moves.push(isMovingWindows())
    return range
  })
  const first = result.current.endDate!.getTime()
  await waitFor(() =>
    expect(result.current.endDate!.getTime()).toBeGreaterThan(first)
  )
  // The first render is the range picked; the render that moved it is a move.
  expect(moves[0]).toBe(false)
  expect(moves[moves.length - 1]).toBe(true)
})

test('a custom range never moves', async () => {
  const from = new Date('2026-10-01T00:00:00Z')
  const to = new Date('2026-10-02T00:00:00Z')
  const { result } = renderHook(() =>
    useAdvancingDateRange(
      { quickFilter: 'custom', dateRange: { from, to } },
      20
    )
  )
  await new Promise((resolve) => setTimeout(resolve, 80))
  expect(result.current.startDate).toBe(from)
  expect(result.current.endDate).toBe(to)
})

test('without an interval a relative range moves only when advanced', async () => {
  const filter = { quickFilter: '24hours', dateRange: undefined } as const
  const { result } = renderHook(() => useAdvancingDateRange(filter, null))
  const first = result.current.endDate!.getTime()
  await new Promise((resolve) => setTimeout(resolve, 40))
  expect(result.current.endDate!.getTime()).toBe(first)

  act(() => result.current.advance())

  expect(result.current.endDate!.getTime()).toBeGreaterThan(first)
})

test('advancing a custom range keeps its dates', () => {
  const from = new Date('2026-10-01T00:00:00Z')
  const to = new Date('2026-10-02T00:00:00Z')
  const { result } = renderHook(() =>
    useAdvancingDateRange(
      { quickFilter: 'custom', dateRange: { from, to } },
      null
    )
  )

  act(() => result.current.advance())

  expect(result.current.startDate).toBe(from)
  expect(result.current.endDate).toBe(to)
})
