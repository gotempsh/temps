// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  getDateRangeFromFilter,
  type AnalyticsDateFilter,
} from './useAnalyticsDateRange'
import { moveWindows } from '@/lib/refresh'

/** How often a relative range ("last 24 hours") moves forward to now. */
export const RANGE_ADVANCE_MS = 5 * 60_000

/**
 * The date range of a filter, kept current: a relative range is recomputed
 * every `intervalMs` while the tab is visible, and as soon as it becomes
 * visible again, so a page left open keeps showing new visits without a
 * refresh button. Each advance changes the range, and so the queries keyed on
 * it, which is what reloads the data. A custom range never moves.
 *
 * `advance` moves a relative range to now on demand (a Refresh button). Pass
 * `intervalMs: null` for a page that only moves when asked.
 */
export function useAdvancingDateRange(
  dateFilter: AnalyticsDateFilter,
  intervalMs: number | null = RANGE_ADVANCE_MS
): {
  startDate: Date | undefined
  endDate: Date | undefined
  advance: () => void
} {
  const [tick, setTick] = useState(0)
  const relative = dateFilter.quickFilter !== 'custom'
  const advance = useCallback(() => setTick((t) => t + 1), [])

  useEffect(() => {
    if (!relative || intervalMs === null) return
    const advance = () => {
      if (document.visibilityState !== 'visible') return
      moveWindows(() => setTick((t) => t + 1))
    }
    const timer = window.setInterval(advance, intervalMs)
    document.addEventListener('visibilitychange', advance)
    return () => {
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', advance)
    }
  }, [relative, intervalMs])

  const range = useMemo(
    () => {
      void tick
      return getDateRangeFromFilter(dateFilter)
    },
    // `tick` is the advance: it recomputes `now` for a relative range.
    [dateFilter, tick]
  )
  return useMemo(() => ({ ...range, advance }), [range, advance])
}
