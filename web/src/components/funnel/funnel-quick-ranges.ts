// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Preset windows for the funnel view.
 *
 * The funnel only had a two-month calendar popover, so answering "how did this
 * convert today" meant hand-picking two dates on every visit. These are the
 * windows people actually ask for, one click each; the calendar stays for
 * anything else.
 *
 * Bounds are exact instants rather than whole days — the metrics endpoint
 * filters on `timestamp >= start AND timestamp <= end`, so "24h" means the
 * trailing 24 hours, not "since midnight".
 */

export interface FunnelQuickRange {
  key: string
  /** Button text. */
  label: string
  /** Accessible name and tooltip, since the button text is an abbreviation. */
  description: string
  hours: number
}

export const FUNNEL_QUICK_RANGES: FunnelQuickRange[] = [
  { key: '24h', label: '24h', description: 'Last 24 hours', hours: 24 },
  { key: '7d', label: '7d', description: 'Last 7 days', hours: 24 * 7 },
  { key: '30d', label: '30d', description: 'Last 30 days', hours: 24 * 30 },
  { key: '90d', label: '90d', description: 'Last 90 days', hours: 24 * 90 },
]

/** The window the funnel opens on, matching the previous hard-coded default. */
export const FUNNEL_DEFAULT_RANGE_KEY = '30d'

export function funnelQuickRange(key: string): FunnelQuickRange | undefined {
  return FUNNEL_QUICK_RANGES.find((range) => range.key === key)
}

/**
 * Resolve a preset to a concrete range ending now. `now` is injected so this
 * stays pure and testable.
 */
export function funnelQuickRangeBounds(
  key: string,
  now: Date = new Date()
): { from: Date; to: Date } | undefined {
  const range = funnelQuickRange(key)
  if (!range) return undefined
  return {
    from: new Date(now.getTime() - range.hours * 60 * 60 * 1000),
    to: now,
  }
}
