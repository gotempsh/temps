// Shared metric formatting + histogram helpers.
//
// Extracted from MetricsExplorer.tsx so the dashboard tiles render values and
// percentiles identically to the explorer. Keep these in lockstep with the
// explorer's local copies (the explorer predates this module and still inlines
// them; both compute the same thing).

import type { MetricBucket } from '@/api/client'
import type { ThresholdLineSeries } from '@/components/charts/threshold-line-chart'
import { format } from 'date-fns'

/** The aggregation tokens the backend's `MetricAggregation::parse` accepts. */
export type AggKind =
  | 'avg'
  | 'sum'
  | 'min'
  | 'max'
  | 'count'
  | 'rate'
  | 'p50'
  | 'p90'
  | 'p95'
  | 'p99'

export const AGGREGATIONS: { value: AggKind; label: string }[] = [
  { value: 'avg', label: 'Average' },
  { value: 'sum', label: 'Sum' },
  { value: 'min', label: 'Min' },
  { value: 'max', label: 'Max' },
  { value: 'count', label: 'Count' },
  { value: 'rate', label: 'Rate / sec' },
  { value: 'p50', label: 'p50' },
  { value: 'p90', label: 'p90' },
  { value: 'p95', label: 'p95' },
  { value: 'p99', label: 'p99' },
]

export function aggregationLabel(agg: string): string {
  return AGGREGATIONS.find((a) => a.value === agg)?.label ?? agg
}

export function formatBucketLabel(bucket: string): string {
  const d = new Date(bucket)
  if (Number.isNaN(d.getTime())) return bucket
  return format(d, 'MMM d HH:mm')
}

export function formatMetricValue(v: number): string {
  if (!Number.isFinite(v)) return '—'
  const abs = Math.abs(v)
  if (abs >= 1_000_000) return `${(v / 1_000_000).toFixed(2)}M`
  if (abs >= 1_000) return `${(v / 1_000).toFixed(2)}k`
  if (abs >= 1) return v.toFixed(2)
  return v.toPrecision(3)
}

export function percentileFromAgg(agg: string): number {
  switch (agg) {
    case 'p50':
      return 0.5
    case 'p90':
      return 0.9
    case 'p95':
      return 0.95
    case 'p99':
      return 0.99
    default:
      return 0.5
  }
}

/**
 * Quantile from an explicit-bucket histogram via linear interpolation within the
 * bucket that crosses the target rank. `bounds` are ascending upper bounds;
 * `counts` has length `bounds.length + 1` (the trailing entry is the +Inf
 * bucket). Returns 0 for an empty histogram.
 *
 * When the recorded `min`/`max` are passed, the first and last *populated*
 * buckets are bounded to the observed range. Without this, a single over-wide
 * bucket (e.g. all sub-millisecond samples in a `0–5ms` bucket) spreads the
 * quantiles across the full bucket width — so p99 reads ~5 while the true max
 * is ~0.5. Bounding to [min, max] keeps quantiles inside the observed values.
 */
export function histogramQuantile(
  bounds: number[],
  counts: number[],
  q: number,
  min?: number | null,
  max?: number | null,
): number {
  const total = counts.reduce((a, c) => a + c, 0)
  if (total === 0) return 0
  const target = q * total
  let firstPop = -1
  let lastPop = -1
  for (let i = 0; i < counts.length; i++) {
    if (counts[i] > 0) {
      if (firstPop < 0) firstPop = i
      lastPop = i
    }
  }
  let cumulative = 0
  for (let i = 0; i < counts.length; i++) {
    const next = cumulative + counts[i]
    if (next >= target && counts[i] > 0) {
      let lower = i === 0 ? 0 : bounds[i - 1]
      let upper =
        i < bounds.length ? bounds[i] : (bounds[bounds.length - 1] ?? lower)
      if (i === firstPop && min != null) lower = Math.max(lower, min)
      if (i === lastPop && max != null) upper = Math.min(upper, max)
      if (upper < lower) upper = lower
      const within = (target - cumulative) / counts[i]
      const value = lower + (upper - lower) * within
      if (min != null && max != null) {
        return Math.min(Math.max(value, min), max)
      }
      return value
    }
    cumulative = next
  }
  return max ?? bounds[bounds.length - 1] ?? 0
}

// ── Group-by breakdown pivot ─────────────────────────────────────────────────

/** Chart readability cap from ADR-026 Phase 2 — beyond this, rank by summed
 * value over the window and drop the tail (surfaced as a "N more" note). Shared
 * by the dashboard tile (MetricTile) and the ad-hoc explorer (MetricsExplorer)
 * so a breakdown renders identically in both. */
export const MAX_BREAKDOWN_SERIES = 20

export type BreakdownModel = {
  kind: 'grouped'
  data: Record<string, unknown>[]
  series: ThresholdLineSeries[]
  droppedCount: number
}

/**
 * Pivot flat, per-`series_key` buckets into wide-format rows (one row per
 * timestamp, one column per distinct label combination) for the chart.
 *
 * `dataKey` is an index-based id (`s0`, `s1`, …), NOT the raw `series_key`
 * JSON — recharts/ChartContainer turn `dataKey` into a CSS custom-property
 * name (`--color-s0`), and a JSON string's quotes/brackets would corrupt that
 * generated stylesheet. The human-readable `key=value` join only ever appears
 * in the `label` shown to users.
 */
export function buildBreakdownData(
  buckets: MetricBucket[],
  isPercentile: boolean,
  aggregation: string,
): BreakdownModel {
  const rowsByBucket = new Map<string, Record<string, unknown>>()
  const totalByGroupKey = new Map<string, number>()
  const labelByGroupKey = new Map<string, string>()
  const valuesByGroupKey = new Map<string, Map<string, number>>()

  for (const b of buckets) {
    const hs = b.histogram_summary
    const value =
      isPercentile && hs && hs.bounds.length > 0
        ? histogramQuantile(
            hs.bounds,
            hs.bucket_counts,
            percentileFromAgg(aggregation),
            hs.min,
            hs.max,
          )
        : (b.value ?? b.avg_value)

    const pairs = [...(b.series_key ?? [])].sort(([a], [b2]) =>
      a.localeCompare(b2),
    )
    const groupKey = JSON.stringify(pairs)
    labelByGroupKey.set(
      groupKey,
      pairs.length > 0
        ? pairs.map(([k, v]) => `${k}=${v}`).join(', ')
        : '(no labels)',
    )
    totalByGroupKey.set(
      groupKey,
      (totalByGroupKey.get(groupKey) ?? 0) + (value ?? 0),
    )

    let byBucket = valuesByGroupKey.get(groupKey)
    if (!byBucket) {
      byBucket = new Map()
      valuesByGroupKey.set(groupKey, byBucket)
    }
    byBucket.set(b.bucket, value ?? 0)

    if (!rowsByBucket.has(b.bucket)) {
      rowsByBucket.set(b.bucket, {
        bucket: b.bucket,
        label: formatBucketLabel(b.bucket),
      })
    }
  }

  const rankedGroupKeys = Array.from(totalByGroupKey.entries())
    .sort((a, b) => b[1] - a[1])
    .map(([k]) => k)
  const keptGroupKeys = rankedGroupKeys.slice(0, MAX_BREAKDOWN_SERIES)
  const droppedCount = rankedGroupKeys.length - keptGroupKeys.length

  const series: ThresholdLineSeries[] = keptGroupKeys.map((groupKey, i) => ({
    dataKey: `s${i}`,
    label: labelByGroupKey.get(groupKey) ?? groupKey,
  }))

  const rows = Array.from(rowsByBucket.entries())
    .sort((a, b) => new Date(a[0]).getTime() - new Date(b[0]).getTime())
    .map(([bucket, baseRow]) => {
      const row = { ...baseRow }
      keptGroupKeys.forEach((groupKey, i) => {
        const v = valuesByGroupKey.get(groupKey)?.get(bucket)
        if (v !== undefined) row[`s${i}`] = v
      })
      return row
    })

  return { kind: 'grouped', data: rows, series, droppedCount }
}
