export type ProxyRangePreset = '1h' | '6h' | '24h' | '7d'
export type ProxyRangeValue = ProxyRangePreset | 'custom'

export const PROXY_RANGE_PRESETS: { value: ProxyRangePreset; label: string }[] =
  [
    { value: '1h', label: '1h' },
    { value: '6h', label: '6h' },
    { value: '24h', label: '24h' },
    { value: '7d', label: '7d' },
  ]

/** Matches `temps_core::time_window::MAX_WINDOW_DAYS` (unscoped reads). */
export const PROXY_MAX_WINDOW_DAYS = 7

/** Matches `temps_core::time_window::MAX_WINDOW_DAYS_SCOPED`. */
export const PROXY_MAX_WINDOW_DAYS_SCOPED = 30

const PRESET_SECONDS: Record<ProxyRangePreset, number> = {
  '1h': 3_600,
  '6h': 21_600,
  '24h': 86_400,
  '7d': 604_800,
}

export type ResolvedProxyWindow = {
  startIso: string
  endIso: string
  durationSeconds: number
  /** Timescale/ClickHouse `bucket_interval` matching node-metric resolution. */
  bucketInterval: string
  /** Preset passed to `/nodes/{id}/metrics`. Undefined for a custom range. */
  rangeParam: ProxyRangePreset | undefined
  /** Include the calendar date on the chart x-axis. */
  showDate: boolean
}

/** Matches `temps_core::time_window::MAX_SERIES_POINTS`. */
export const PROXY_MAX_CHART_POINTS = 1_000

/** Bucket width matching the node-metrics endpoint's per-range resolution. */
export function pickProxyBucketInterval(durationSeconds: number): string {
  if (durationSeconds <= 3_600) return '1 minute'
  if (durationSeconds <= 21_600) return '5 minutes'
  if (durationSeconds <= 86_400) return '15 minutes'
  return '1 hour'
}

export function formatProxyTimeLabel(iso: string, showDate: boolean): string {
  const d = new Date(iso)
  if (showDate) {
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  }
  return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

/**
 * Resolve the proxy dashboard window.
 *
 * An incomplete custom range (Custom clicked, dates not yet picked) falls
 * back to the 24h preset so charts keep a valid query rather than hanging
 * empty.
 */
export function resolveProxyWindow(
  range: ProxyRangeValue,
  custom: { from?: Date; to?: Date } | undefined,
  now: Date
): ResolvedProxyWindow {
  if (range === 'custom' && custom?.from && custom?.to) {
    const start = custom.from
    const end = custom.to.getTime() >= custom.from.getTime() ? custom.to : custom.from
    const durationSeconds = Math.max(
      1,
      (end.getTime() - start.getTime()) / 1000
    )
    return {
      startIso: start.toISOString(),
      endIso: end.toISOString(),
      durationSeconds,
      bucketInterval: pickProxyBucketInterval(durationSeconds),
      rangeParam: undefined,
      showDate: durationSeconds > 86_400,
    }
  }

  const preset: ProxyRangePreset = range === 'custom' ? '24h' : range
  const durationSeconds = PRESET_SECONDS[preset]
  const start = new Date(now.getTime() - durationSeconds * 1000)
  return {
    startIso: start.toISOString(),
    endIso: now.toISOString(),
    durationSeconds,
    bucketInterval: pickProxyBucketInterval(durationSeconds),
    rangeParam: preset,
    showDate: preset === '7d',
  }
}

export function proxyWindowTooWide(
  window: ResolvedProxyWindow,
  projectScoped: boolean
): boolean {
  const maxDays = projectScoped
    ? PROXY_MAX_WINDOW_DAYS_SCOPED
    : PROXY_MAX_WINDOW_DAYS
  return window.durationSeconds > maxDays * 86_400
}
