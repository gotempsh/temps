// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Apdex thresholds offered on the operations page, in milliseconds. */
export const APDEX_THRESHOLD_OPTIONS = ['100', '250', '500', '1000', '2000']

/** The server's default threshold, kept out of the URL when chosen. */
export const DEFAULT_APDEX_THRESHOLD = '500'

export type ApdexRating =
  'excellent' | 'good' | 'fair' | 'poor' | 'unacceptable'

/**
 * The conventional Apdex bands: 0.94 and up is excellent, 0.85 good, 0.70
 * fair, 0.50 poor, anything lower unacceptable.
 */
export function apdexRating(score: number): ApdexRating {
  if (score >= 0.94) return 'excellent'
  if (score >= 0.85) return 'good'
  if (score >= 0.7) return 'fair'
  if (score >= 0.5) return 'poor'
  return 'unacceptable'
}

/** The threshold a URL asked for, or the default when it is not offered. */
export function apdexThresholdFromParam(value: string | null): string {
  return value && APDEX_THRESHOLD_OPTIONS.includes(value)
    ? value
    : DEFAULT_APDEX_THRESHOLD
}

/** "120 satisfied, 30 tolerating, 6 frustrated" for a tooltip. */
export function apdexBreakdown(row: {
  satisfied_count?: number | null
  tolerating_count?: number | null
  frustrated_count?: number | null
}): string | null {
  if (row.satisfied_count == null) return null
  return `${row.satisfied_count.toLocaleString()} satisfied, ${(
    row.tolerating_count ?? 0
  ).toLocaleString()} tolerating, ${(
    row.frustrated_count ?? 0
  ).toLocaleString()} frustrated`
}
