// Shared metric formatting + histogram helpers.
//
// Extracted from MetricsExplorer.tsx so the dashboard tiles render values and
// percentiles identically to the explorer. Keep these in lockstep with the
// explorer's local copies (the explorer predates this module and still inlines
// them; both compute the same thing).

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
 */
export function histogramQuantile(
  bounds: number[],
  counts: number[],
  q: number,
): number {
  const total = counts.reduce((a, c) => a + c, 0)
  if (total === 0) return 0
  const target = q * total
  let cumulative = 0
  for (let i = 0; i < counts.length; i++) {
    const next = cumulative + counts[i]
    if (next >= target && counts[i] > 0) {
      const lower = i === 0 ? 0 : bounds[i - 1]
      const upper =
        i < bounds.length ? bounds[i] : (bounds[bounds.length - 1] ?? lower)
      const within = (target - cumulative) / counts[i]
      return lower + (upper - lower) * within
    }
    cumulative = next
  }
  return bounds[bounds.length - 1] ?? 0
}
