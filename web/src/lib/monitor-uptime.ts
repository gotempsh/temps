// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for the uptime monitor views (monitor detail + monitor list).
 *
 * The bucketed-status endpoint returns every bucket of the range; one no
 * check landed in is `"unknown"` and must render grey, never green. Uptime is
 * the share of *monitored* time that was operational, and the time nothing
 * was checking (`unknown_seconds`) is surfaced next to it instead of being
 * silently counted as up.
 */

export type BucketTone = 'operational' | 'degraded' | 'down' | 'unknown'

/**
 * Map a bucket or check status to a tone. Buckets say `"down"` while check
 * rows say `"major_outage"`; both are red. Anything else (including
 * `"unknown"`, a bucket without checks) is grey.
 */
export function bucketTone(status: string | null | undefined): BucketTone {
  switch (status) {
    case 'operational':
      return 'operational'
    case 'degraded':
    case 'partial_outage':
      return 'degraded'
    case 'down':
    case 'major_outage':
      return 'down'
    default:
      return 'unknown'
  }
}

export const BUCKET_TONE_CLASS: Record<BucketTone, string> = {
  operational: 'bg-green-500',
  degraded: 'bg-yellow-500',
  down: 'bg-red-500',
  unknown: 'bg-gray-300 dark:bg-gray-600',
}

export const BUCKET_TONE_LABEL: Record<BucketTone, string> = {
  operational: 'Operational',
  degraded: 'Degraded',
  down: 'Major Outage',
  unknown: 'No checks',
}

/** "3h 5m", "45m", "30s" — durations as an operator reads them. */
export function formatDurationShort(seconds: number): string {
  const s = Math.max(0, Math.round(seconds))
  if (s < 60) return `${s}s`
  const minutes = Math.floor(s / 60)
  if (minutes < 60) return `${minutes}m`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  if (hours < 48) return rest > 0 ? `${hours}h ${rest}m` : `${hours}h`
  const days = Math.floor(hours / 24)
  const restHours = hours % 24
  return restHours > 0 ? `${days}d ${restHours}h` : `${days}d`
}

/** Uptime for display; `—` when nothing in the range was monitored. */
export function formatUptime(percentage: number | null | undefined): string {
  if (percentage == null || !Number.isFinite(percentage)) return '—'
  return `${percentage.toFixed(2)}%`
}

/** Below this, unknown time is jitter between checks, not a gap worth flagging. */
const UNKNOWN_NOTE_MIN_SECONDS = 60

/**
 * The caveat shown under the uptime figure when part of the range has no
 * check data, e.g. "No checks for 3h (12.5% of the range)". `null` when the
 * whole range was monitored.
 */
export function unknownTimeNote(status: {
  monitored_seconds?: number | null
  unknown_seconds?: number | null
}): string | null {
  const unknown = status.unknown_seconds ?? 0
  if (unknown < UNKNOWN_NOTE_MIN_SECONDS) return null
  const total = unknown + (status.monitored_seconds ?? 0)
  const share = total > 0 ? (unknown / total) * 100 : 100
  const shareText =
    share >= 99.95 ? 'the whole range' : `${share.toFixed(1)}% of the range`
  return `No checks for ${formatDurationShort(unknown)} (${shareText})`
}
