// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { subDays } from 'date-fns'
import type { DateRange } from 'react-day-picker'

export const QUICK_FILTERS = [
  { label: 'Last hour', value: 'lasthour' },
  { label: 'Today', value: 'today' },
  { label: 'Yesterday', value: 'yesterday' },
  { label: 'Last 24 hours', value: '24hours' },
  { label: 'Last 7 Days', value: '7days' },
  { label: 'Last 30 Days', value: '30days' },
  { label: 'Custom', value: 'custom' },
] as const

export type QuickFilter = (typeof QUICK_FILTERS)[number]['value']

export interface AnalyticsDateFilter {
  quickFilter: QuickFilter
  dateRange: DateRange | undefined
}

export function getDateRangeFromFilter(dateFilter: AnalyticsDateFilter): {
  startDate: Date | undefined
  endDate: Date | undefined
} {
  const now = new Date()
  if (dateFilter.quickFilter === 'custom' && dateFilter.dateRange) {
    return {
      startDate: dateFilter.dateRange.from,
      endDate: dateFilter.dateRange.to,
    }
  }

  switch (dateFilter.quickFilter) {
    case 'lasthour': {
      const oneHourAgo = new Date(now)
      oneHourAgo.setHours(oneHourAgo.getHours() - 1)
      return {
        startDate: oneHourAgo,
        endDate: now,
      }
    }
    case 'today':
      return {
        startDate: new Date(now.setHours(0, 0, 0, 0)),
        endDate: new Date(now.setHours(23, 59, 59, 999)),
      }
    case 'yesterday': {
      const yesterday = new Date(now)
      yesterday.setDate(yesterday.getDate() - 1)
      return {
        startDate: new Date(yesterday.setHours(0, 0, 0, 0)),
        endDate: new Date(yesterday.setHours(23, 59, 59, 999)),
      }
    }
    case '24hours': {
      const twentyFourHoursAgo = new Date(now)
      twentyFourHoursAgo.setHours(twentyFourHoursAgo.getHours() - 24)
      return {
        startDate: twentyFourHoursAgo,
        endDate: now,
      }
    }
    case '7days':
      return {
        startDate: subDays(now, 7),
        endDate: now,
      }
    case '30days':
      return {
        startDate: subDays(now, 30),
        endDate: now,
      }
    default:
      return {
        startDate: subDays(now, 7),
        endDate: now,
      }
  }
}
