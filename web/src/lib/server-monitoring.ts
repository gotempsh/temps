// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for the control-plane "Server" monitoring section.
 *
 * Kept free of React so the rate maths, the disk projection and the
 * threshold logic can be unit tested without rendering charts.
 */

import type { MetricDataPoint } from '@/api/client/types.gen'
import type { ProxyRangePreset } from '@/lib/proxy-metrics-window'

/** The control-plane node always has id 0 (`CONTROL_PLANE_NODE_ID`). */
export const CONTROL_PLANE_NODE_ID = 0

/** Bucket width the store uses for each preset (`duration_to_step`). */
export const STEP_SECONDS: Record<ProxyRangePreset, number> = {
  '1h': 60,
  '6h': 300,
  '24h': 900,
  '7d': 3600,
}

export type UsageTone = 'good' | 'warn' | 'poor'

/** Warn / critical lines, as a share of the resource. */
export type UsageThresholds = { warn: number; poor: number }
export const CPU_THRESHOLDS: UsageThresholds = { warn: 80, poor: 95 }
export const MEMORY_THRESHOLDS: UsageThresholds = { warn: 85, poor: 95 }
export const DISK_THRESHOLDS: UsageThresholds = { warn: 80, poor: 90 }

export function usageTone(
  percent: number | null | undefined,
  t: UsageThresholds
): UsageTone {
  if (percent == null || !Number.isFinite(percent)) return 'good'
  if (percent >= t.poor) return 'poor'
  if (percent >= t.warn) return 'warn'
  return 'good'
}

/** One row of a merged multi-series chart: ISO time, axis label, one value per key. */
export type ChartRow = { time: string; label: string } & Record<
  string,
  string | number | null
>

/**
 * Merge several metric series (each `[{time, value}]`) into rows keyed by
 * timestamp so recharts can draw them on one axis. Missing values are left
 * out so a series with a gap breaks instead of dropping to zero.
 */
export function mergeSeries(
  series: { key: string; points: MetricDataPoint[] | undefined }[],
  label: (iso: string) => string
): ChartRow[] {
  const rows = new Map<string, ChartRow>()
  for (const { key, points } of series) {
    for (const p of points ?? []) {
      if (Number.isNaN(Date.parse(p.time))) continue
      const row: ChartRow = rows.get(p.time) ?? {
        time: p.time,
        label: label(p.time),
      }
      row[key] = p.value
      rows.set(p.time, row)
    }
  }
  return [...rows.values()].sort((a, b) => a.time.localeCompare(b.time))
}

/**
 * Convert the per-bucket *increases* the range endpoint returns for a
 * `*_total` counter into a per-second rate. The bucket width is the gap to
 * the previous bucket (the first bucket uses the gap to the next one; all
 * buckets in a range share one step), and a lone point falls back to
 * `fallbackStepSeconds`.
 */
export function toRatePerSecond(
  points: MetricDataPoint[] | undefined,
  fallbackStepSeconds: number
): MetricDataPoint[] {
  if (!points || points.length === 0) return []
  const times = points.map((p) => Date.parse(p.time))
  const stepFor = (i: number): number => {
    if (points.length === 1) return fallbackStepSeconds
    const gap = i > 0 ? times[i] - times[i - 1] : times[i + 1] - times[i]
    const secs = gap / 1000
    return Number.isFinite(secs) && secs > 0 ? secs : fallbackStepSeconds
  }
  return points.map((p, i) => ({
    time: p.time,
    value: Math.max(0, p.value) / stepFor(i),
  }))
}

/** Highest point of a series and when it happened. */
export function peakOf(
  points: MetricDataPoint[] | undefined
): { time: string; value: number } | null {
  let best: { time: string; value: number } | null = null
  for (const p of points ?? []) {
    if (!Number.isFinite(p.value)) continue
    if (!best || p.value > best.value) best = { time: p.time, value: p.value }
  }
  return best
}

// ── Disk projection ────────────────────────────────────────────────────

/** Fewest samples a projection is fitted through. */
export const MIN_PROJECTION_SAMPLES = 6
/**
 * Shortest stretch of history a projection is extrapolated from. Disk usage
 * moves in bursts — an image pull, a build cache, a log rotation — so a few
 * minutes of samples turn one 2 GB pull into "growing 1 TB/day". Six hours
 * averages over several such bursts.
 */
export const MIN_PROJECTION_SPAN_MS = 6 * 3_600_000
/**
 * How well the line must explain the samples (R²) before its slope is
 * reported as growth. Below this, usage is going up and down, not filling.
 */
export const MIN_PROJECTION_FIT = 0.5

export type DiskProjection =
  /** Too little history to extrapolate from. */
  | {
      kind: 'insufficient'
      samples: number
      /** Milliseconds between the first and last sample. */
      spanMs: number
    }
  /** Flat, shrinking, or fluctuating without a trend. */
  | { kind: 'steady'; spanMs: number }
  /**
   * The fitted rate is implausible for this disk (it would fill the whole
   * volume from empty in under a day) — almost always a burst, not a trend.
   */
  | { kind: 'unreliable'; spanMs: number; bytesPerDay: number }
  | {
      kind: 'growing'
      spanMs: number
      /** Bytes per day from a least-squares line through the window. */
      bytesPerDay: number
      /** Days from the last sample until the critical line is reached. */
      daysToLine: number
      /** Days from the last sample until the disk is full. */
      daysToFull: number
    }

/**
 * Fit a line through the disk-used samples and say when it reaches the
 * critical line and when it fills — but only when the history can support
 * it: at least {@link MIN_PROJECTION_SAMPLES} samples spanning
 * {@link MIN_PROJECTION_SPAN_MS}, a line that explains the samples, and a
 * rate the disk could physically sustain. Returns `null` without a total.
 */
export function projectDisk(
  points: MetricDataPoint[] | undefined,
  totalBytes: number | null | undefined,
  criticalPercent = DISK_THRESHOLDS.poor
): DiskProjection | null {
  if (!totalBytes || !(totalBytes > 0)) return null
  const samples = (points ?? []).filter(
    (p) => Number.isFinite(p.value) && !Number.isNaN(Date.parse(p.time))
  )
  const xs = samples.map((p) => Date.parse(p.time))
  const ys = samples.map((p) => p.value)
  const n = xs.length
  const spanMs = n > 1 ? Math.max(...xs) - Math.min(...xs) : 0
  if (n < MIN_PROJECTION_SAMPLES || spanMs < MIN_PROJECTION_SPAN_MS) {
    return { kind: 'insufficient', samples: n, spanMs }
  }

  const x0 = xs[0]
  const mx = xs.reduce((a, x) => a + (x - x0), 0) / n
  const my = ys.reduce((a, y) => a + y, 0) / n
  let sxx = 0
  let sxy = 0
  let syy = 0
  for (let i = 0; i < n; i++) {
    const dx = xs[i] - x0 - mx
    const dy = ys[i] - my
    sxx += dx * dx
    sxy += dx * dy
    syy += dy * dy
  }
  if (sxx === 0) return { kind: 'insufficient', samples: n, spanMs }
  const bytesPerDay = (sxy / sxx) * 86_400_000
  // Coefficient of determination. A perfectly flat series (syy = 0) has no
  // trend to explain and is steady by definition.
  const fit = syy === 0 ? 0 : (sxy * sxy) / (sxx * syy)
  if (bytesPerDay <= 0 || fit < MIN_PROJECTION_FIT) {
    return { kind: 'steady', spanMs }
  }
  if (bytesPerDay > totalBytes) {
    return { kind: 'unreliable', spanMs, bytesPerDay }
  }

  const last = ys[n - 1]
  const daysTo = (target: number) => Math.max(0, (target - last) / bytesPerDay)
  return {
    kind: 'growing',
    spanMs,
    bytesPerDay,
    daysToLine: daysTo(totalBytes * (criticalPercent / 100)),
    daysToFull: daysTo(totalBytes),
  }
}

/** The disk card's caption: free space plus what the projection supports. */
export function diskProjectionCaption(
  freeBytes: number,
  projection: DiskProjection | null
): string {
  const free = `${formatBytesDecimal(freeBytes)} free`
  if (!projection) return free
  switch (projection.kind) {
    case 'insufficient':
      return `${free} · collecting history to project growth (needs ${MIN_PROJECTION_SPAN_MS / 3_600_000} h)`
    case 'steady':
      return `${free} · no steady growth over the last ${formatAge(projection.spanMs / 1000)}`
    case 'unreliable':
      return `${free} · usage jumped recently; too irregular to project`
    case 'growing':
      return `${free} · growing ${formatBytesDecimal(projection.bytesPerDay)}/day over the last ${formatAge(projection.spanMs / 1000)}, full in ${formatDays(projection.daysToFull)}`
  }
}

/** "44 days", "3 months": a projection in the unit an operator plans in. */
export function formatDays(d: number): string {
  if (!Number.isFinite(d)) return 'not growing'
  if (d < 1) return 'less than a day'
  if (d < 90) return `${Math.round(d)} day${Math.round(d) === 1 ? '' : 's'}`
  if (d < 730) return `${Math.round(d / 30)} months`
  return `${Math.round(d / 365)} years`
}

// ── Formatting ─────────────────────────────────────────────────────────

/** Binary-unit byte formatter, e.g. `1.89 GiB`. */
export function formatBytesBinary(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes)) return '—'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB']
  let v = Math.max(0, bytes)
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  const decimals = i === 0 ? 0 : v >= 100 ? 0 : v >= 10 ? 1 : 2
  return `${v.toFixed(decimals)} ${units[i]}`
}

/** Decimal-unit byte formatter used for disk sizes, e.g. `13.9 GB`. */
export function formatBytesDecimal(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes)) return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  let v = Math.max(0, bytes)
  let i = 0
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000
    i++
  }
  const decimals = i === 0 ? 0 : v >= 100 ? 0 : v >= 10 ? 1 : 2
  return `${v.toFixed(decimals)} ${units[i]}`
}

/** Throughput formatter, e.g. `2.4 MiB/s`. */
export function formatBytesPerSecond(
  bytesPerSecond: number | null | undefined
): string {
  if (bytesPerSecond == null || !Number.isFinite(bytesPerSecond)) return '—'
  return `${formatBytesBinary(bytesPerSecond)}/s`
}

/**
 * Compact throughput for a narrow y axis: `296M`, `1.2k`, `512`. The unit is
 * on the panel and the tooltip prints the full `MiB/s` form.
 */
export function formatRateTick(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond)) return ''
  const v = Math.max(0, bytesPerSecond)
  if (v >= 1024 ** 3) return `${(v / 1024 ** 3).toFixed(1)}G`
  if (v >= 1024 ** 2)
    return `${(v / 1024 ** 2).toFixed(v >= 10 * 1024 ** 2 ? 0 : 1)}M`
  if (v >= 1024) return `${(v / 1024).toFixed(v >= 10 * 1024 ? 0 : 1)}k`
  return `${Math.round(v)}`
}

export function formatPercent(
  v: number | null | undefined,
  digits = 1
): string {
  if (v == null || !Number.isFinite(v)) return '—'
  return `${v.toFixed(digits)}%`
}

/** Clamp a used/total pair to a 0–100 percentage for progress bars. */
export function usagePercent(
  used: number | null | undefined,
  total: number | null | undefined
): number {
  if (used == null || total == null || !(total > 0)) return 0
  return Math.min(100, Math.max(0, (used / total) * 100))
}

/** Seconds as the words a caption uses ("12 s", "3 min", "2 h"). */
export function formatAge(seconds: number): string {
  if (seconds < 90) return `${Math.round(seconds)} s`
  if (seconds < 5400) return `${Math.round(seconds / 60)} min`
  if (seconds < 172_800) return `${(seconds / 3600).toFixed(1)} h`
  return `${(seconds / 86_400).toFixed(1)} d`
}

/**
 * Whether a metrics-store error is the endpoint's 503 "not enabled" answer,
 * as opposed to a real failure. Drives the onboarding card.
 */
export function isMetricsUnavailable(err: unknown): boolean {
  const problem = err as
    { status?: number; detail?: string; title?: string } | undefined
  if (problem?.status === 503) return true
  const msg = `${problem?.detail ?? ''} ${problem?.title ?? ''}`.toLowerCase()
  return msg.includes('not enabled') || msg.includes('unavailable')
}

/** Slices of the Docker disk-usage breakdown, in fixed order. */
export const DOCKER_USAGE_SLICES = [
  { key: 'images', label: 'Images' },
  { key: 'containers', label: 'Containers' },
  { key: 'volumes', label: 'Volumes' },
  { key: 'build_cache', label: 'Build cache' },
] as const
export type DockerUsageSliceKey = (typeof DOCKER_USAGE_SLICES)[number]['key']
