// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Parse a period string into start/end ISO dates and a human label.
 *
 * Supports:
 *   - "today"         — since midnight
 *   - "<n>h"          — last N hours (e.g. 1h, 6h, 24h, 48h)
 *   - "<n>d"          — last N days  (e.g. 7d, 30d, 90d)
 *   - "<n>m"          — last N months (e.g. 1m, 3m, 6m)
 */
export function parsePeriod(period: string): { startDate: string; endDate: string; label: string } {
  const now = new Date()
  const endDate = now.toISOString()

  if (period === 'today') {
    const start = new Date(now.getFullYear(), now.getMonth(), now.getDate())
    return { startDate: start.toISOString(), endDate, label: 'today' }
  }

  const match = period.match(/^(\d+)(h|d|m)$/)
  if (!match) {
    throw new Error(
      `Invalid period "${period}". Use: today, <n>h (hours), <n>d (days), <n>m (months). Examples: 1h, 6h, 24h, 7d, 30d, 3m`
    )
  }

  const n = parseInt(match[1]!, 10)
  const unit = match[2]!

  let start: Date
  let label: string

  switch (unit) {
    case 'h':
      start = new Date(now.getTime() - n * 60 * 60 * 1000)
      label = n === 1 ? 'last hour' : `last ${n} hours`
      break
    case 'd':
      start = new Date(now.getTime() - n * 24 * 60 * 60 * 1000)
      label = n === 1 ? 'last day' : `last ${n} days`
      break
    case 'm':
      start = new Date(now)
      start.setMonth(start.getMonth() - n)
      label = n === 1 ? 'last month' : `last ${n} months`
      break
    default:
      throw new Error(`Unknown unit "${unit}"`)
  }

  return { startDate: start.toISOString(), endDate, label }
}
