// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** What a log tile charts per bucket: matching lines, or a numeric field. */
export const LOG_TILE_FUNCTIONS = [
  { value: 'count', label: 'Matching lines' },
  { value: 'avg', label: 'Average of' },
  { value: 'sum', label: 'Sum of' },
  { value: 'max', label: 'Maximum of' },
  { value: 'p50', label: 'p50 of' },
  { value: 'p95', label: 'p95 of' },
  { value: 'p99', label: 'p99 of' },
  { value: 'count_distinct', label: 'Distinct values of' },
] as const
export type LogTileFunction = (typeof LOG_TILE_FUNCTIONS)[number]['value']

const FUNCTION_VALUES = LOG_TILE_FUNCTIONS.map((f) => f.value) as string[]

/** The server caps a series at these many buckets (`log_series.rs`). */
const MAX_COUNT_BUCKETS = 240
const MAX_NUMERIC_BUCKETS = 60

/** Bucket widths a chart reads well at, smallest first. */
const STEPS = [
  60, 120, 300, 600, 900, 1800, 3600, 7200, 10800, 21600, 43200, 86400,
]

/**
 * The line index's attribute key grammar (`[A-Za-z_][A-Za-z0-9_.]{0,63}`);
 * the dashboard API refuses anything else.
 */
export function isLogAttributeKey(key: string): boolean {
  return /^[A-Za-z_][A-Za-z0-9_.]{0,63}$/.test(key.trim())
}

/**
 * Why a log tile cannot be charted, or `null` when it can. A numeric field
 * is aggregated by the line index, which holds no message text, so a saved
 * search with a text filter can only be charted as a line count.
 */
export function logTileProblem(
  fn: LogTileFunction,
  searchFiltersText: boolean
): string | null {
  if (fn === 'count' || !searchFiltersText) return null
  return 'This saved search filters message text, which only a line count can chart. Choose Matching lines, or a search without a text filter.'
}

/** Whether a saved search's query filters message text. */
export function searchFiltersText(
  query: { text?: string | null } | null | undefined
): boolean {
  return !!query?.text
}

/** `count`, or `<fn>:<attribute>`, the way a tile stores it. */
export function formatLogTileMetric(
  fn: LogTileFunction,
  attribute: string
): string {
  return fn === 'count' ? 'count' : `${fn}:${attribute.trim()}`
}

/** The stored `metric` back into its parts; anything unknown reads as a count. */
export function parseLogTileMetric(metric: string | null | undefined): {
  fn: LogTileFunction
  attribute: string
} {
  if (!metric || metric === 'count') return { fn: 'count', attribute: '' }
  const index = metric.indexOf(':')
  const fn = index > 0 ? metric.slice(0, index) : ''
  if (!FUNCTION_VALUES.includes(fn) || fn === 'count')
    return { fn: 'count', attribute: '' }
  return { fn: fn as LogTileFunction, attribute: metric.slice(index + 1) }
}

/**
 * The narrowest bucket that keeps the window under the server's bucket cap:
 * 240 buckets for a count, 60 for a numeric field.
 */
export function logTileBucketSecs(
  fromIso: string,
  toIso: string,
  metric: string
): number {
  const span = Math.max(1, (Date.parse(toIso) - Date.parse(fromIso)) / 1000)
  const cap = metric === 'count' ? MAX_COUNT_BUCKETS : MAX_NUMERIC_BUCKETS
  const needed = Math.ceil(span / cap)
  return (
    STEPS.find((step) => step >= needed) ?? Math.ceil(needed / 86400) * 86400
  )
}

/** A short label for the tile's badge. */
export function logTileMetricLabel(metric: string): string {
  const { fn, attribute } = parseLogTileMetric(metric)
  if (fn === 'count') return 'lines'
  return `${fn} ${attribute}`
}
