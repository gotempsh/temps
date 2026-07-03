import { ProjectResponse } from '@/api/client'
import { queryMetricsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ThresholdLineChart,
  ThresholdLineSeries,
} from '@/components/charts/threshold-line-chart'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import {
  aggregationLabel,
  buildBreakdownData,
  formatBucketLabel,
  formatMetricValue,
  histogramQuantile,
  percentileFromAgg,
} from './metric-format'
import { StatusDot } from './alert-format'
import {
  dynamicFiringSeriesCount,
  STATUS_META,
  useAlertStatus,
} from './alert-status'
import { useAnomalyBand } from './use-anomaly-band'
import {
  LabelFilterChips,
  serializeLabelFilters,
  tuplesToLabelFilters,
} from './LabelFilterBuilder'

interface MetricTileProps {
  project: ProjectResponse
  metricName: string
  /** Aggregation token (avg|sum|min|max|count|rate|pNN). */
  aggregation: string
  /** Optional display title; falls back to the metric name. */
  title?: string | null
  /**
   * AND-combined label equality filters scoping this tile's chart, in the
   * `DashboardTile.label_filters` ordered-tuple shape. Empty/absent = no
   * filtering (the whole metric, today's behavior).
   */
  labelFilters?: [string, string][]
  /**
   * Up to 2 label keys to break the chart down by, in `DashboardTile.
   * group_by` order — renders one line per distinct value combination
   * instead of a single aggregate line. Empty/absent = today's behavior.
   */
  groupBy?: string[]
  /** ISO start bound — memoize upstream so the query key stays stable. */
  fromIso: string
  /** ISO end bound — memoize upstream so the query key stays stable. */
  toIso: string
  /** Canonical ClickHouse bucket interval (e.g. "15 minutes"). */
  bucketInterval: string
  /** Chart body height in px. */
  height?: number
}

/**
 * A single dashboard metric tile: fetches a time-bucketed series for one metric
 * + aggregation and renders it as a compact line chart. Reuses the exact query
 * shape and histogram-percentile recomputation from MetricsExplorer so saved
 * dashboards render identically to the ad-hoc explorer.
 *
 * Time bounds are passed in as already-memoized ISO strings — callers MUST NOT
 * pass an inline `new Date()` (that bug was fixed in MetricsExplorer: a fresh
 * Date every render changes the query key and spins React Query into an
 * infinite refetch loop).
 */
export function MetricTile({
  project,
  metricName,
  aggregation,
  title,
  labelFilters,
  groupBy,
  fromIso,
  toIso,
  bucketInterval,
  height = 160,
}: MetricTileProps) {
  const isPercentile = aggregation.startsWith('p')

  // The query param is a comma-separated `key=value` string (same format
  // MetricsExplorer's ad-hoc filter builder sends) — recomputed from the
  // ordered-tuple prop, not memoized, since it's a cheap derived string and
  // its VALUE (not the array reference) is what matters for the query key.
  const labelFiltersParam =
    serializeLabelFilters(tuplesToLabelFilters(labelFilters ?? [])) || undefined
  const groupByParam =
    groupBy && groupBy.length > 0 ? groupBy.join(',') : undefined

  // Datadog-style: a status dot + a toned line when a rule on this exact
  // (metric, aggregation) is firing. Cached `listAlerts` — no extra fetch.
  const alertStatus = useAlertStatus(project.id)
  const status = alertStatus.statusFor(metricName, aggregation)
  const dynamicFiringCount = dynamicFiringSeriesCount(
    alertStatus.rulesFor(metricName, aggregation),
  )
  const lineTone =
    status === 'alert' ? 'poor' : status === 'warn' ? 'warn' : 'primary'

  // Datadog-style anomaly overlay: the expected-range band + breach markers,
  // identical to the explorer drill-in. Only renders when an anomaly rule covers
  // this metric and the backtest had enough history.
  const { bandSeries, mergeBand } = useAnomalyBand({
    project,
    metricName,
    aggregation,
    fromIso,
    toIso,
    enabled: metricName.length > 0,
  })

  const q = useQuery({
    ...queryMetricsOptions({
      query: {
        project_id: project.id,
        metric_name: metricName,
        start_time: fromIso,
        end_time: toIso,
        bucket_interval: bucketInterval,
        // AggKind strings map directly to the backend's MetricAggregation::parse.
        aggregation,
        label_filters: labelFiltersParam,
        group_by: groupByParam,
        limit: 500,
      },
    }),
    enabled: !!project.id && metricName.length > 0,
  })

  const buckets = q.data?.data ?? []
  const isHistogram = buckets.some((b) => b.histogram_summary)
  const isGrouped = !!groupByParam

  // Anomaly bands are per-metric aggregates, not per-series — they don't
  // compose with a breakdown (same reason ADR-026 keeps dynamic alerting and
  // anomaly detection apart), so a grouped tile skips the overlay entirely
  // rather than merging it into the wrong series.
  const chartModel = useMemo(() => {
    if (isGrouped) return buildBreakdownData(buckets, isPercentile, aggregation)
    return {
      kind: 'single' as const,
      data: mergeBand(
        buckets.map((b) => {
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
          return { bucket: b.bucket, label: formatBucketLabel(b.bucket), value }
        }),
      ),
    }
  }, [buckets, isPercentile, aggregation, mergeBand, isGrouped])

  const data = chartModel.data
  const series: ThresholdLineSeries | ThresholdLineSeries[] =
    chartModel.kind === 'grouped'
      ? chartModel.series
      : { dataKey: 'value', label: aggregationLabel(aggregation), tone: lineTone }
  const droppedSeriesCount =
    chartModel.kind === 'grouped' ? chartModel.droppedCount : 0
  const latest =
    chartModel.kind === 'single' && data.length
      ? (data[data.length - 1].value as number | null)
      : null
  const displayTitle = title?.trim() ? title : metricName

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-card p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="flex min-w-0 items-center gap-1.5">
          {status && (
            <StatusDot
              level={status}
              pulse
              title={
                dynamicFiringCount != null
                  ? `${dynamicFiringCount} series breaching`
                  : `Alert rule: ${STATUS_META[status].label}`
              }
            />
          )}
          <span
            className="truncate font-mono text-xs font-medium"
            title={metricName}
          >
            {displayTitle}
          </span>
        </span>
        <div className="flex shrink-0 items-center gap-1">
          {isHistogram && (
            <Badge variant="outline" className="text-[10px]">
              histogram
            </Badge>
          )}
          <Badge variant="secondary" className="text-[10px]">
            {aggregationLabel(aggregation)}
          </Badge>
        </div>
      </div>

      <LabelFilterChips filters={labelFilters ?? []} groupBy={groupBy} />

      {q.isPending ? (
        <Skeleton className="w-full" style={{ height }} />
      ) : q.isError ? (
        <div
          className="flex items-center justify-center text-center text-xs text-rose-500"
          style={{ height }}
        >
          Failed to load metric
        </div>
      ) : data.length === 0 ? (
        <div
          className="flex items-center justify-center text-center text-xs text-muted-foreground"
          style={{ height }}
        >
          No data in range
        </div>
      ) : (
        <ThresholdLineChart
          data={data}
          xKey="label"
          series={series}
          bandSeries={isGrouped ? undefined : bandSeries}
          height={height}
          tooltipValueFormatter={(v) => formatMetricValue(v)}
          yTickFormatter={(v) => formatMetricValue(v)}
        />
      )}

      {isGrouped ? (
        droppedSeriesCount > 0 && (
          <p className="text-[11px] text-muted-foreground">
            {droppedSeriesCount} more series not shown
          </p>
        )
      ) : (
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span className="text-[10px] uppercase tracking-wide">latest</span>
          <span className="font-mono tabular-nums">
            {latest != null ? formatMetricValue(latest) : '—'}
          </span>
        </div>
      )}
    </div>
  )
}
