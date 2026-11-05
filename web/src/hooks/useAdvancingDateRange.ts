// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState } from 'react'
import {
  getDateRangeFromFilter,
  type AnalyticsDateFilter,
} from './useAnalyticsDateRange'

/** How often a relative range ("last 24 hours") moves forward to now. */
export const RANGE_ADVANCE_MS = 5 * 60_000

/**
 * The date range of a filter, kept current: a relative range is recomputed
 * every `intervalMs` while the tab is visible, and as soon as it becomes
 * visible again, so a page left open keeps showing new visits without a
 * refresh button. Each advance changes the range, and so the queries keyed on
 * it, which is what reloads the data. A custom range never moves.
 */
export function useAdvancingDateRange(
  dateFilter: AnalyticsDateFilter,
  intervalMs = RANGE_ADVANCE_MS
): { startDate: Date | undefined; endDate: Date | undefined } {
  const [tick, setTick] = useState(0)
  const relative = dateFilter.quickFilter !== 'custom'

  useEffect(() => {
    if (!relative) return
    const advance = () => {
      if (document.visibilityState === 'visible') setTick((t) => t + 1)
    }
    const timer = window.setInterval(advance, intervalMs)
    document.addEventListener('visibilitychange', advance)
    return () => {
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', advance)
    }
  }, [relative, intervalMs])

  return useMemo(
    () => {
      void tick
      return getDateRangeFromFilter(dateFilter)
    },
    // `tick` is the advance: it recomputes `now` for a relative range.
    [dateFilter, tick]
  )
}
