// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Average completion time formatting.
 *
 * The old display collapsed the value to a single rounded unit — `Math.round(s
 * / 3600)` + "h" — so 3,540s and 5,340s both read "1h" despite being half an
 * hour apart, and the actual number the API returned was nowhere on screen.
 * That is a lot of precision to throw away for a metric whose whole point is
 * comparison between funnels.
 *
 * So: a readable compound value for scanning, plus the exact seconds beside it.
 */

export interface FunnelDuration {
  /** Compound, human-scannable: `1h 29m`, `5m 12s`, `42s`. */
  primary: string
  /** The raw figure the API returned, e.g. `5,340 seconds`. */
  exact: string
}

function round(seconds: number): number {
  // Negative or non-finite input would render as garbage; treat it as no data.
  if (!Number.isFinite(seconds) || seconds <= 0) return 0
  return Math.round(seconds)
}

/**
 * Two most-significant units only — `1h 29m`, never `1h 29m 3s`. Below a
 * minute there is only one unit to show anyway.
 */
function compound(totalSeconds: number): string {
  if (totalSeconds === 0) return '0s'

  const hours = Math.floor(totalSeconds / 3600)
  const minutes = Math.floor((totalSeconds % 3600) / 60)
  const seconds = totalSeconds % 60

  if (hours > 0) {
    return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`
  }
  if (minutes > 0) {
    return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`
  }
  return `${seconds}s`
}

export function formatFunnelDuration(seconds: number): FunnelDuration {
  const total = round(seconds)
  return {
    primary: compound(total),
    exact: `${total.toLocaleString()} ${total === 1 ? 'second' : 'seconds'}`,
  }
}
