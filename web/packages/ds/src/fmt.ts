// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Small `Intl`-backed formatters. No hand-written number/date formatting
// anywhere else in the package or in consumer code built on it — locale,
// pluralization and relative-time rules are exactly the kind of thing that
// looks right in a demo and breaks in a non-US locale.

const numberFormatCache = new Map<string, Intl.NumberFormat>()

function numberFormat(options: Intl.NumberFormatOptions): Intl.NumberFormat {
  const key = JSON.stringify(options)
  let formatter = numberFormatCache.get(key)
  if (!formatter) {
    formatter = new Intl.NumberFormat(undefined, options)
    numberFormatCache.set(key, formatter)
  }
  return formatter
}

/** `1234` -> "1,234" (locale thousands separators). */
export function fmtNumber(value: number): string {
  return numberFormat({}).format(value)
}

/** `0.1234` -> "12.3%". */
export function fmtPercent(value: number, digits = 1): string {
  return numberFormat({
    style: 'percent',
    minimumFractionDigits: 0,
    maximumFractionDigits: digits,
  }).format(value)
}

/** `1536` -> "1.5 KB". Binary (1024-based) units, matching `formatBytes` elsewhere in the app. */
export function fmtBytes(bytes: number, digits = 1): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  const exponent = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)))
  const value = bytes / 1024 ** exponent
  return `${numberFormat({ maximumFractionDigits: exponent === 0 ? 0 : digits }).format(value)} ${units[exponent]}`
}

/** A millisecond duration to the largest unit that reads naturally (e.g. "1.2s", "500ms"). // audit-ignore: prose, not a styling literal */
export function fmtDuration(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  if (Math.abs(ms) < 1000) return `${numberFormat({ maximumFractionDigits: 0 }).format(ms)}ms`
  const seconds = ms / 1000
  if (Math.abs(seconds) < 60) return `${numberFormat({ maximumFractionDigits: 1 }).format(seconds)}s`
  const minutes = seconds / 60
  if (Math.abs(minutes) < 60) return `${numberFormat({ maximumFractionDigits: 1 }).format(minutes)}m`
  const hours = minutes / 60
  return `${numberFormat({ maximumFractionDigits: 1 }).format(hours)}h`
}

const relativeFormat = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })
const RELATIVE_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 60 * 60 * 24 * 365],
  ['month', 60 * 60 * 24 * 30],
  ['week', 60 * 60 * 24 * 7],
  ['day', 60 * 60 * 24],
  ['hour', 60 * 60],
  ['minute', 60],
  ['second', 1],
]

/** `Date` -> "3 hours ago" / "in 2 days". */
export function fmtRelativeTime(date: Date | string | number, now: Date | number = Date.now()): string {
  const then = new Date(date).getTime()
  const nowMs = typeof now === 'number' ? now : now.getTime()
  const deltaSeconds = (then - nowMs) / 1000
  for (const [unit, secondsInUnit] of RELATIVE_UNITS) {
    if (Math.abs(deltaSeconds) >= secondsInUnit || unit === 'second') {
      return relativeFormat.format(Math.round(deltaSeconds / secondsInUnit), unit)
    }
  }
  return relativeFormat.format(0, 'second')
}

/** `Date` -> "Sep 17, 2026, 3:04 PM" (locale date + time, no seconds). */
export function fmtDateTime(date: Date | string | number): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(date))
}

/** `Date` -> "Sep 17, 2026". */
export function fmtDate(date: Date | string | number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' }).format(new Date(date))
}
