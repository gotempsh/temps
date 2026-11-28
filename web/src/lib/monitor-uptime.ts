// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for the uptime monitor views (monitor detail + monitor list).
 *
 * The bucketed-status endpoint returns every bucket of the range with how
 * many of its checks were successful, degraded or failed; the timeline draws
 * that mix (`bucketParts`), and a bucket no check landed in renders grey,
 * never green. Uptime is
 * the share of *monitored* time that was operational, and the time nothing
 * was checking (`unknown_seconds`) is surfaced next to it instead of being
 * silently counted as up.
 */

export type BucketTone = 'operational' | 'degraded' | 'down' | 'unknown'

export const BUCKET_TONE_CLASS: Record<BucketTone, string> = {
  operational: 'bg-green-500',
  degraded: 'bg-yellow-500',
  down: 'bg-red-500',
  unknown: 'bg-gray-300 dark:bg-gray-600',
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

/** What a single check found: a bucket mixes these in proportion. */
export type CheckTone = Exclude<BucketTone, 'unknown'>

/** How a check's result is named in the timeline, its tooltip and legend. */
export const CHECK_LABEL: Record<CheckTone, string> = {
  operational: 'Successful',
  degraded: 'Degraded',
  down: 'Failed',
}

export interface BucketPart {
  tone: CheckTone
  count: number
  /** Of the checks in the bucket, 0 to 1. */
  share: number
}

/**
 * The checks of one bucket by what they found, successful first, each with
 * its share. A bucket's colour used to be its worst check alone, so an hour
 * of 5 successes and 36 degraded checks drew all yellow: the parts let the
 * timeline draw the mix. Parts with no checks are left out; a bucket
 * without checks has none.
 */
export function bucketParts(bucket: {
  operational_count?: number | null
  degraded_count?: number | null
  down_count?: number | null
}): BucketPart[] {
  const counts: [CheckTone, number][] = [
    ['operational', Math.max(0, bucket.operational_count ?? 0)],
    ['degraded', Math.max(0, bucket.degraded_count ?? 0)],
    ['down', Math.max(0, bucket.down_count ?? 0)],
  ]
  const total = counts.reduce((sum, [, count]) => sum + count, 0)
  if (total === 0) return []
  return counts
    .filter(([, count]) => count > 0)
    .map(([tone, count]) => ({ tone, count, share: count / total }))
}

/**
 * "5 successful, 36 degraded of 41 checks"; "41 checks, all degraded" when
 * they agree; "No checks" for a bucket nothing checked.
 */
export function bucketSummary(parts: readonly BucketPart[]): string {
  const total = parts.reduce((sum, part) => sum + part.count, 0)
  if (total === 0) return 'No checks'
  const noun = total === 1 ? 'check' : 'checks'
  if (parts.length === 1) {
    const found = CHECK_LABEL[parts[0].tone].toLowerCase()
    return total === 1 ? `1 ${found} check` : `${total} checks, all ${found}`
  }
  const listed = parts
    .map((part) => `${part.count} ${CHECK_LABEL[part.tone].toLowerCase()}`)
    .join(', ')
  return `${listed} of ${total} ${noun}`
}

const BUCKET_STEP_MS: Record<string, number> = {
  '1min': 60_000,
  '5min': 300_000,
  hourly: 3_600_000,
  daily: 86_400_000,
}

/**
 * The span a bucket covers, in the reader's time zone: "27 Nov, 20:00–21:00",
 * or the day alone for daily buckets ("27 Nov 2026").
 */
export function bucketRangeLabel(
  bucketStart: string,
  interval: string,
  locale?: string
): string {
  const start = new Date(bucketStart)
  if (Number.isNaN(start.getTime())) return bucketStart
  const day = start.toLocaleDateString(locale, {
    day: 'numeric',
    month: 'short',
    ...(interval === 'daily' ? { year: 'numeric' } : {}),
  })
  const step = BUCKET_STEP_MS[interval]
  if (interval === 'daily' || step === undefined) return day
  const end = new Date(start.getTime() + step)
  // Across a daylight-saving change two hours read alike ("01:00–01:00"):
  // then each end names its offset.
  const zoned = start.getTimezoneOffset() !== end.getTimezoneOffset()
  const time = (date: Date) =>
    date.toLocaleTimeString(locale, {
      hour: '2-digit',
      minute: '2-digit',
      hourCycle: 'h23',
      ...(zoned ? { timeZoneName: 'shortOffset' } : {}),
    })
  return `${day}, ${time(start)}–${time(end)}`
}

/** "6 ms median · 12 ms p95", or the average when percentiles are missing. */
export function bucketResponseTime(bucket: {
  p50_response_time_ms?: number | null
  p95_response_time_ms?: number | null
  avg_response_time_ms?: number | null
}): string {
  const ms = (value: number) => `${Math.round(value)} ms`
  if (
    bucket.p50_response_time_ms != null &&
    bucket.p95_response_time_ms != null
  )
    return `${ms(bucket.p50_response_time_ms)} median · ${ms(bucket.p95_response_time_ms)} p95`
  if (bucket.avg_response_time_ms != null)
    return `${ms(bucket.avg_response_time_ms)} average`
  return '—'
}
