// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { format, isSameDay } from 'date-fns'

export interface ChartDateRange {
  from: Date
  to: Date
}

export function formatChartDateRange(range: ChartDateRange): string {
  if (isSameDay(range.from, range.to)) {
    return `${format(range.from, 'MMM d, yyyy · HH:mm')} → ${format(range.to, 'HH:mm')}`
  }
  return `${format(range.from, 'MMM d, yyyy · HH:mm')} → ${format(range.to, 'MMM d, yyyy · HH:mm')}`
}

function chartValueToDate(value: unknown): Date | null {
  if (value instanceof Date) {
    return Number.isNaN(value.getTime()) ? null : value
  }
  if (typeof value !== 'number' && typeof value !== 'string') return null

  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? null : date
}

export function orderedChartDateRange(
  first: unknown,
  second: unknown
): ChartDateRange | null {
  const firstDate = chartValueToDate(first)
  const secondDate = chartValueToDate(second)
  if (!firstDate || !secondDate) return null

  const firstTime = firstDate.getTime()
  const secondTime = secondDate.getTime()
  if (firstTime === secondTime) return null

  return firstTime < secondTime
    ? { from: firstDate, to: secondDate }
    : { from: secondDate, to: firstDate }
}
