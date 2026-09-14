// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { format } from 'date-fns'

export const QUICK_TIME_RANGES = {
  '1h': 1,
  '6h': 6,
  '1d': 24,
  '7d': 168,
} as const
export type QuickTimeRange = keyof typeof QUICK_TIME_RANGES
export type DateTimeRangeValue = {
  from: string
  to: string
  preset: QuickTimeRange | 'custom'
}

export function quickTimeRange(
  preset: QuickTimeRange,
  now = Date.now()
): DateTimeRangeValue {
  return {
    preset,
    from: new Date(now - QUICK_TIME_RANGES[preset] * 3600000).toISOString(),
    to: new Date(now).toISOString(),
  }
}

export function localDateTime(value: string): string {
  return format(new Date(value), "yyyy-MM-dd'T'HH:mm")
}

/** datetime-local inputs use the browser's local timezone; API values use UTC. */
export function customTimeRange(
  from: string,
  to: string,
  maxDays: number
): { value: DateTimeRangeValue } | { field: 'from' | 'to'; message: string } {
  const start = Date.parse(from),
    end = Date.parse(to)
  if (
    !Number.isFinite(start) ||
    localDateTime(new Date(start).toISOString()) !== from
  )
    return {
      field: 'from',
      message: 'Enter a valid start date and time in your timezone.',
    }
  if (
    !Number.isFinite(end) ||
    localDateTime(new Date(end).toISOString()) !== to
  )
    return {
      field: 'to',
      message: 'Enter a valid end date and time in your timezone.',
    }
  if (end <= start)
    return { field: 'to', message: 'End time must be after start time.' }
  if (end - start > maxDays * 86400000)
    return {
      field: 'to',
      message: `Choose a time range of ${maxDays} days or less.`,
    }
  return {
    value: {
      preset: 'custom',
      from: new Date(start).toISOString(),
      to: new Date(end).toISOString(),
    },
  }
}
