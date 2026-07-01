// Shared time-range selection for metric views (explorer + dashboards).
//
// Mirrors the RANGE_BUCKET / timeRangeToFrom contract from MetricsExplorer: the
// backend's `translate_bucket_interval` parses these canonical "N unit" strings
// through a strict allowlist, so keep this map in lockstep with it.

export type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

export const TIME_RANGES: { value: TimeRange; label: string }[] = [
  { value: '1h', label: 'Last hour' },
  { value: '6h', label: 'Last 6 hours' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
  { value: '30d', label: 'Last 30 days' },
]

export const RANGE_BUCKET: Record<TimeRange, string> = {
  '1h': '1 minute',
  '6h': '5 minutes',
  '24h': '15 minutes',
  '7d': '1 hour',
  '30d': '6 hours',
}

const RANGE_MS: Record<TimeRange, number> = {
  '1h': 60 * 60 * 1000,
  '6h': 6 * 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
  '30d': 30 * 24 * 60 * 60 * 1000,
}

export function timeRangeToFrom(range: TimeRange, now: number): Date {
  return new Date(now - RANGE_MS[range])
}

export function isTimeRange(v: string | null | undefined): v is TimeRange {
  return !!v && TIME_RANGES.some((t) => t.value === v)
}
