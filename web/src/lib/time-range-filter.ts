// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  type DateTimeRangeValue,
  type QuickTimeRange,
  QUICK_TIME_RANGES,
} from './date-time-range'

/** Serializable range used by existing relative-range filters and shared links. */
export function resolveTimeRange(
  value: string,
  now = Date.now()
): DateTimeRangeValue {
  if (value.startsWith('custom:')) {
    const [from, to] = value.slice(7).split('/')
    if (
      Number.isFinite(Date.parse(from)) &&
      Number.isFinite(Date.parse(to)) &&
      Date.parse(to) > Date.parse(from)
    )
      return { from, to, preset: 'custom' }
  }
  let normalized = value === '24h' ? '1d' : value
  const match = /^(\d+)(m|h|d)$/.exec(normalized)
  let hours = match
    ? Number(match[1]) * ({ m: 1 / 60, h: 1, d: 24 }[match[2]] ?? 1)
    : 24
  if (!Number.isFinite(hours) || hours <= 0 || hours > 3650 * 24) {
    hours = 24
    normalized = '1d'
  }
  if (!match) normalized = '1d'
  return {
    from: new Date(now - hours * 3600000).toISOString(),
    to: new Date(now).toISOString(),
    preset:
      normalized in QUICK_TIME_RANGES
        ? (normalized as QuickTimeRange)
        : 'custom',
  }
}

export function serializeTimeRange(value: DateTimeRangeValue): string {
  return value.preset === 'custom'
    ? `custom:${value.from}/${value.to}`
    : value.preset === '1d'
      ? '24h'
      : value.preset
}
