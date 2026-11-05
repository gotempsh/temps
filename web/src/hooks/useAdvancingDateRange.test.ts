// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, renderHook, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
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
