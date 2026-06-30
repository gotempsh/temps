import {
  EnvironmentResponse,
  HistogramSummary,
  ProjectResponse,
} from '@/api/client'
import {
  getEnvironmentsOptions,
  listMetricNamesOptions,
  listMetricLabelKeysOptions,
  listMetricLabelValuesOptions,
  queryMetricsOptions,
  listAlertsOptions,
  // Backtests an anomaly rule's band over the visible range to shade it.
  previewAlertMutation,
  // Deploy events overlaid as vertical markers — "did a deploy cause this?"
  getProjectDeploymentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ThresholdBand,
  ThresholdBandSeries,
  ThresholdMarker,
} from '@/components/charts/threshold-line-chart'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import { Skeleton } from '@/components/ui/skeleton'
import { EmptyState } from '@/components/ui/empty-state'
import { ThresholdLineChart } from '@/components/charts/threshold-line-chart'
import { comparatorSymbol, StatusDot } from '@/components/metrics/alert-format'
import { MetricCorrelations } from '@/components/metrics/MetricCorrelations'
import {
  STATUS_META,
  statusRank,
  useAlertStatus,
  type AlertStatusLevel,
} from '@/components/metrics/alert-status'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ArrowLeft,
  BarChart3,
  Check,
  ChevronDown,
  ChevronsUpDown,
  Gauge,
  LineChart as LineChartIcon,
  Plus,
  RefreshCw,
  Search,
  SlidersHorizontal,
  Tag,
  X,
} from 'lucide-react'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { useEffect, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

interface MetricsExplorerProps {
  project: ProjectResponse
}

// ── Time ranges ────────────────────────────────────────────────────────────

type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

const TIME_RANGES: { value: TimeRange; label: string }[] = [
  { value: '1h', label: 'Last hour' },
  { value: '6h', label: 'Last 6 hours' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
  { value: '30d', label: 'Last 30 days' },
]

/**
 * Map a time range to a sensible default ClickHouse-side bucket interval.
 *
 * The backend (`query_metrics`) parses these through a strict allowlist
 * (`translate_bucket_interval`), so only the canonical `"N unit"` strings are
 * accepted — keep this map in lockstep with that allowlist.
 */
const RANGE_BUCKET: Record<TimeRange, string> = {
  '1h': '1 minute',
  '6h': '5 minutes',
  '24h': '15 minutes',
  '7d': '1 hour',
  '30d': '6 hours',
}

function timeRangeToFrom(range: TimeRange): Date {
  const now = Date.now()
  const map: Record<TimeRange, number> = {
    '1h': 60 * 60 * 1000,
    '6h': 6 * 60 * 60 * 1000,
    '24h': 24 * 60 * 60 * 1000,
    '7d': 7 * 24 * 60 * 60 * 1000,
    '30d': 30 * 24 * 60 * 60 * 1000,
  }
  return new Date(now - map[range])
}

/**
 * Pick a ClickHouse bucket interval for an arbitrary (custom-range) span,
 * mirroring the thresholds in `RANGE_BUCKET` so a custom 24h looks like the
 * 24h preset. Keep the labels in the same allowlist the backend parses.
 */
function bucketForSpan(spanMs: number): string {
  const hours = spanMs / 3_600_000
  if (hours <= 2) return '1 minute'
  if (hours <= 12) return '5 minutes'
  if (hours <= 48) return '15 minutes'
  if (hours <= 24 * 14) return '1 hour'
  return '6 hours'
}

// ── Aggregation selector ─────────────────────────────────────────────────────
//
// These mirror the FROZEN store-neutral contract from Phase C
// (`MetricAggregation` in temps-otel/src/types.rs). The backend accepts the
// string form via the `aggregation` query param. NOTE: the `aggregation`
// param is NOT yet present in the generated `QueryMetricsData` type — it only
// lands after the SDK is regenerated against the Phase C backend (see the
// REGEN TODO near the query below). Until then the selector is presentational
// and the chart falls back to the average series the current SDK returns.

type AggKind =
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

const AGGREGATIONS: { value: AggKind; label: string }[] = [
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

// ── Label filters ────────────────────────────────────────────────────────────

interface LabelFilter {
  key: string
  value: string
}

/** Serialize label filters to a compact, regen-ready URL string: `k=v,k2=v2`. */
function serializeLabelFilters(filters: LabelFilter[]): string {
  return filters
    .filter((f) => f.key.trim().length > 0)
    .map((f) => `${f.key.trim()}=${f.value.trim()}`)
    .join(',')
}

function parseLabelFilters(raw: string | null): LabelFilter[] {
  if (!raw) return []
  return raw
    .split(',')
    .map((pair) => pair.trim())
    .filter(Boolean)
    .map((pair) => {
      const idx = pair.indexOf('=')
      if (idx === -1) return { key: pair, value: '' }
      return { key: pair.slice(0, idx), value: pair.slice(idx + 1) }
    })
}

// ── Page ─────────────────────────────────────────────────────────────────────

/**
 * OTel metrics explorer — mirrors the TracesList pattern (metric-name
 * selector + filter builder + time range) but renders a time-bucketed line
 * chart instead of a row list.
 *
 * Backend wiring: bound to the generated SDK (`listMetricNames`,
 * `queryMetrics`). The richer Phase C query surface (per-series `group_by`,
 * typed `aggregation`, `metric_type`, `label_filters`, and the richer
 * `OtelMetricBucket` response with `value`/`quantiles`/`series_key`) is
 * SCAFFOLDED here but commented as a REGEN TODO because the generated SDK in
 * this worktree predates the Phase C handler changes. Project rule: always use
 * the generated SDK — never hand-roll a fetch — so the new params stay
 * commented until the SDK is regenerated against a running server:
 *
 *   1. Start the server (see start-temps skill).
 *   2. Mint a local admin API key:  bunx @temps-sdk/cli api-key create ...
 *   3. cd web && bun run openapi-ts        # regenerates src/api/client
 *   4. Revoke the temporary key.
 *
 * After regen the renamed types (OtelMetricsResponse / OtelMetricBucket /
 * OtelMetricNamesResponse) and the new query params become available and the
 * TODO blocks below can be un-commented.
 */
export default function MetricsExplorer({ project }: MetricsExplorerProps) {
  usePageTitle(`Metrics · ${project.name}`)
  const [searchParams, setSearchParams] = useSearchParams()

  // ── URL-backed filter state ──
  const metricName = searchParams.get('metric') ?? ''
  const serviceName = searchParams.get('service') ?? ''
  const timeRange = ((): TimeRange => {
    const r = searchParams.get('range') as TimeRange | null
    return r && TIME_RANGES.some((t) => t.value === r) ? r : '24h'
  })()
  const environmentId = searchParams.get('environment_id')
    ? Number(searchParams.get('environment_id'))
    : null
  const aggregation = ((): AggKind => {
    const a = searchParams.get('agg') as AggKind | null
    return a && AGGREGATIONS.some((x) => x.value === a) ? a : 'avg'
  })()
  const labelFilters = useMemo(
    () => parseLabelFilters(searchParams.get('labels')),
    [searchParams]
  )
  // Custom absolute date range — overrides the relative preset when both bounds
  // are present and valid (`?start`/`?end`, ISO). The backend metric query takes
  // arbitrary start/end, so this is purely a UI concern.
  const customStartStr = searchParams.get('start')
  const customEndStr = searchParams.get('end')
  const isCustom = (() => {
    if (!customStartStr || !customEndStr) return false
    const s = new Date(customStartStr).getTime()
    const e = new Date(customEndStr).getTime()
    return Number.isFinite(s) && Number.isFinite(e) && s < e
  })()

  const [nameSearch, setNameSearch] = useState('')
  // The filter controls collapse by default so the metric grid / chart sits high
  // on the page; the collapsed header still summarizes the active selection.
  const [filtersOpen, setFiltersOpen] = useState(false)

  const patchParams = (mutate: (p: URLSearchParams) => void) => {
    const params = new URLSearchParams(searchParams)
    mutate(params)
    setSearchParams(params, { replace: true })
  }

  const setMetricName = (v: string) =>
    patchParams((p) => (v ? p.set('metric', v) : p.delete('metric')))
  const setServiceName = (v: string) =>
    patchParams((p) => (v ? p.set('service', v) : p.delete('service')))
  const setTimeRange = (v: TimeRange) =>
    patchParams((p) => {
      // Selecting a preset always exits custom mode.
      p.delete('start')
      p.delete('end')
      if (v === '24h') p.delete('range')
      else p.set('range', v)
    })
  // Apply an absolute range (from the date picker, or freezing the current
  // window). Clears the relative preset.
  const setCustomRange = (from: Date, to: Date) =>
    patchParams((p) => {
      p.set('start', from.toISOString())
      p.set('end', to.toISOString())
      p.delete('range')
    })
  const setEnvironmentId = (v: number | null) =>
    patchParams((p) =>
      v == null
        ? p.delete('environment_id')
        : p.set('environment_id', String(v))
    )
  const setAggregation = (v: AggKind) =>
    patchParams((p) => (v === 'avg' ? p.delete('agg') : p.set('agg', v)))
  const setLabelFilters = (next: LabelFilter[]) =>
    patchParams((p) => {
      const s = serializeLabelFilters(next)
      s ? p.set('labels', s) : p.delete('labels')
    })

  // ── Environments (for the env selector, mirrors TracesList) ──
  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })

  // ── Metric-name catalogue ──
  const namesQuery = useQuery({
    ...listMetricNamesOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })

  const allNames = namesQuery.data?.names ?? []
  const filteredNames = useMemo(() => {
    const q = nameSearch.trim().toLowerCase()
    if (!q) return allNames
    return allNames.filter((n) => n.toLowerCase().includes(q))
  }, [allNames, nameSearch])

  // ── Time-bucketed series ──
  // Both bounds are memoized on `timeRange` so they stay STABLE across renders.
  // Using `new Date()` inline would change the query key every render and spin
  // React Query into an infinite refetch loop.
  const fromDate = useMemo(
    () => (isCustom ? new Date(customStartStr!) : timeRangeToFrom(timeRange)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [isCustom, customStartStr, timeRange]
  )
  const toDate = useMemo(
    () => (isCustom ? new Date(customEndStr!) : new Date()),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [isCustom, customEndStr, timeRange]
  )
  const fromIso = fromDate.toISOString()
  const toIso = toDate.toISOString()
  // Preset spans use their tuned bucket; a custom span derives one from its width.
  const bucketInterval = isCustom
    ? bucketForSpan(toDate.getTime() - fromDate.getTime())
    : RANGE_BUCKET[timeRange]
  const selectedEnv = environments?.find((e) => e.id === environmentId) ?? null

  const metricsQuery = useQuery({
    ...queryMetricsOptions({
      query: {
        project_id: project.id,
        metric_name: metricName || undefined,
        service_name: serviceName || undefined,
        environment: selectedEnv?.name || undefined,
        start_time: fromIso,
        end_time: toIso,
        bucket_interval: bucketInterval,
        // The AggKind strings (avg/sum/min/max/count/rate/pNN) map directly to
        // the backend's `MetricAggregation::parse`.
        aggregation,
        label_filters: serializeLabelFilters(labelFilters) || undefined,
        limit: 1000,
      },
    }),
    // Only fetch once a concrete metric is chosen — querying every metric at
    // once would be expensive and meaningless to chart.
    enabled: !!project.id && metricName.length > 0,
  })

  // ── Alert thresholds for the charted metric ──
  // Overlay any alert rule defined on the selected metric as a horizontal
  // reference line, so the explorer visually shows where a rule would fire.
  const alertsQuery = useQuery({
    ...listAlertsOptions({ query: { project_id: project.id } }),
    enabled: !!project.id && metricName.length > 0,
  })
  const thresholds = useMemo<ThresholdBand[]>(() => {
    if (!metricName) return []
    return (alertsQuery.data?.data ?? [])
      .filter((r) => r.enabled && r.metric_name === metricName)
      .flatMap((r) => {
        // Only static rules have a single horizontal threshold to overlay.
        const cfg = r.detection_config
        if (cfg.kind !== 'static') return []
        return [
          {
            value: cfg.threshold,
            tone: r.severity === 'critical' ? 'poor' : 'warn',
            label: `${comparatorSymbol(cfg.comparator)} ${cfg.threshold}`,
          } satisfies ThresholdBand,
        ]
      })
  }, [alertsQuery.data, metricName])

  // Anomaly band overlay: for an enabled anomaly rule on this metric, backtest
  // its band over the visible range and shade the expected region.
  const anomalyRule = useMemo(
    () =>
      (alertsQuery.data?.data ?? []).find(
        (r) =>
          r.enabled &&
          r.metric_name === metricName &&
          r.detection_config.kind === 'anomaly'
      ) ?? null,
    [alertsQuery.data, metricName]
  )
  const bandPreview = useMutation({ ...previewAlertMutation() })
  const { mutate: previewBand, reset: resetBand } = bandPreview
  // Backtest with the DISPLAYED aggregation (not the rule's), so the band always
  // tracks the line on screen and shows even when you're viewing a different
  // aggregation than the rule alerts on — it's the same detector params, just
  // applied to the series you're looking at.
  const bandKey = anomalyRule
    ? `${anomalyRule.id}|${aggregation}|${anomalyRule.window_secs}|${fromIso}|${toIso}|${JSON.stringify(anomalyRule.detection_config)}`
    : ''
  useEffect(() => {
    if (!anomalyRule) {
      resetBand()
      return
    }
    previewBand({
      body: {
        project_id: project.id,
        metric_name: metricName,
        aggregation,
        window_secs: anomalyRule.window_secs,
        detection_config: anomalyRule.detection_config,
        start_time: fromIso,
        end_time: toIso,
      },
    })
    // bandKey captures every input; mutate/reset are stable refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bandKey])

  // Datadog-style anomaly overlay: a time-varying expected-range band with the
  // breaching points marked. Present only when an anomaly rule covers this
  // metric and the backtest had enough history. The per-bucket values are merged
  // into the chart data below (chartDataWithBand).
  const bandSeries = useMemo<ThresholdBandSeries | undefined>(() => {
    if (
      !anomalyRule ||
      !bandPreview.data?.sufficient ||
      (bandPreview.data?.points?.length ?? 0) === 0
    )
      return undefined
    return {
      lowerKey: 'bandLower',
      spanKey: 'bandSpan',
      breachKey: 'bandBreach',
      tone: anomalyRule.severity === 'critical' ? 'poor' : 'warn',
    }
  }, [anomalyRule, bandPreview.data])

  const buckets = metricsQuery.data?.data ?? []

  // Map the `MetricBucket` rows into the chart point shape. `value` carries the
  // requested aggregation; for histogram metrics the percentile aggregations are
  // recomputed client-side from the per-bucket `histogram_summary` layout, since
  // the server's scalar quantile runs over the synthetic mean rather than the
  // true distribution.
  const isPercentile = aggregation.startsWith('p')
  const chartData = useMemo(
    () =>
      buckets.map((b) => {
        const hs = b.histogram_summary
        const value =
          isPercentile && hs && hs.bounds.length > 0
            ? histogramQuantile(
                hs.bounds,
                hs.bucket_counts,
                percentileFromAgg(aggregation),
                hs.min,
                hs.max
              )
            : (b.value ?? b.avg_value)
        return {
          bucket: b.bucket,
          value,
          avg_value: b.avg_value,
          min_value: b.min_value,
          max_value: b.max_value,
          count: b.count,
        }
      }),
    [buckets, isPercentile, aggregation]
  )

  // Merge the anomaly band onto each chart bucket. The backtest buckets by the
  // rule's window, which may not line up 1:1 with the chart's interval, so align
  // each chart point to the NEAREST band point by timestamp. `bandBreach` carries
  // the value only where the point left the band, so the chart can mark it.
  const chartDataWithBand = useMemo(() => {
    const pts = bandPreview.data?.points ?? []
    if (!bandSeries || pts.length === 0) return chartData
    const bandTs = pts.map((p) => new Date(p.bucket).getTime())
    return chartData.map((d) => {
      const t = new Date(d.bucket).getTime()
      let best = 0
      let bestDiff = Infinity
      for (let i = 0; i < bandTs.length; i++) {
        const diff = Math.abs(bandTs[i] - t)
        if (diff < bestDiff) {
          bestDiff = diff
          best = i
        }
      }
      const p = pts[best]
      return {
        ...d,
        bandLower: p.lower,
        bandSpan: Math.max(0, p.upper - p.lower),
        bandBreach: p.breaching ? d.value : null,
      }
    })
  }, [chartData, bandPreview.data, bandSeries])

  // ── Deploy markers ──
  // Overlay deploy events that fall inside the visible window as vertical lines,
  // snapped to the nearest chart bucket. "Did a deploy cause this?" is the first
  // triage question, and Temps owns the deploy pipeline.
  const deploysQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: {
        per_page: 50,
        ...(environmentId != null ? { environment_id: environmentId } : {}),
      },
    }),
    enabled: !!project.id && metricName.length > 0,
  })
  const deployMarkers = useMemo<ThresholdMarker[]>(() => {
    const deploys = deploysQuery.data?.deployments ?? []
    if (chartData.length === 0 || deploys.length === 0) return []
    const fromMs = fromDate.getTime()
    const toMs = toDate.getTime()
    // Timestamps come back in seconds; normalise to ms.
    const toMs10 = (n: number) => (n < 1e12 ? n * 1000 : n)
    const bucketMs = chartData.map((p) => new Date(p.bucket).getTime())
    const markers: ThresholdMarker[] = []
    for (const d of deploys) {
      const at = d.finished_at ?? d.started_at ?? d.created_at
      const ts = toMs10(at)
      if (ts < fromMs || ts > toMs) continue
      // Snap to the nearest bucket (the categorical x axis can't take a raw ts).
      let best = 0
      let bestDiff = Infinity
      for (let i = 0; i < bucketMs.length; i++) {
        const diff = Math.abs(bucketMs[i] - ts)
        if (diff < bestDiff) {
          bestDiff = diff
          best = i
        }
      }
      markers.push({
        x: formatBucketLabel(chartData[best].bucket),
        label: d.commit_hash ? d.commit_hash.slice(0, 7) : 'deploy',
        title: d.commit_message ?? undefined,
      })
    }
    return markers
  }, [deploysQuery.data, chartData, fromDate, toDate])

  // Most recent histogram snapshot in range (for the distribution panel). Using
  // the latest bucket avoids re-summing cumulative snapshots across time.
  const latestHist = useMemo(() => {
    for (let i = buckets.length - 1; i >= 0; i--) {
      const hs = buckets[i].histogram_summary
      if (hs && hs.bounds.length > 0) return hs
    }
    return null
  }, [buckets])

  const aggLabel =
    AGGREGATIONS.find((a) => a.value === aggregation)?.label ?? 'Average'
  // Collapsed-header summary: what's currently narrowing the view.
  const rangeLabel = isCustom
    ? 'Custom range'
    : (TIME_RANGES.find((r) => r.value === timeRange)?.label ?? 'Last 24 hours')
  const envLabel = selectedEnv?.name ?? 'All environments'
  const activeFilterCount =
    (serviceName.trim() ? 1 : 0) +
    labelFilters.filter((f) => f.key.trim().length > 0).length

  return (
    <div className="flex w-full flex-col gap-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <Gauge className="size-5 text-muted-foreground" />
            <h1 className="text-lg font-semibold tracking-tight">Metrics</h1>
          </div>
          <p className="text-sm text-muted-foreground">
            Explore OpenTelemetry metrics for {project.name}.
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            namesQuery.refetch()
            if (metricName) metricsQuery.refetch()
          }}
          className="gap-1.5 self-start"
        >
          <RefreshCw
            className={
              metricsQuery.isFetching || namesQuery.isFetching
                ? 'size-3.5 animate-spin'
                : 'size-3.5'
            }
          />
          Refresh
        </Button>
      </div>

      {/* Filter bar — collapsed by default; the header summarizes the active
          aggregation, environment, range, and any service/label filters. */}
      <Collapsible
        open={filtersOpen}
        onOpenChange={setFiltersOpen}
        className="rounded-lg border border-border bg-card"
      >
        <CollapsibleTrigger className="flex w-full items-center justify-between gap-2 px-3 py-2.5 text-left text-sm transition-colors hover:bg-muted/40">
          <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <SlidersHorizontal className="size-4 shrink-0 text-muted-foreground" />
            <span className="font-medium">Filters</span>
            <span className="text-muted-foreground">·</span>
            <span className="text-muted-foreground">{aggLabel}</span>
            <span className="text-muted-foreground">·</span>
            <span className="text-muted-foreground">{envLabel}</span>
            <span className="text-muted-foreground">·</span>
            <span className="text-muted-foreground">{rangeLabel}</span>
            {activeFilterCount > 0 && (
              <Badge variant="secondary" className="ml-1 shrink-0">
                {activeFilterCount} filter{activeFilterCount === 1 ? '' : 's'}
              </Badge>
            )}
          </span>
          <ChevronDown
            className={[
              'size-4 shrink-0 text-muted-foreground transition-transform',
              filtersOpen ? 'rotate-180' : '',
            ].join(' ')}
          />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="flex flex-col gap-3 border-t border-border p-3">
            <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-end">
              {/* Metric-name selector with inline search */}
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <label className="text-xs font-medium text-muted-foreground">
                  Metric
                </label>
                <Select value={metricName} onValueChange={setMetricName}>
                  <SelectTrigger className="w-full font-mono sm:w-[280px]">
                    <SelectValue placeholder="Select a metric…" />
                  </SelectTrigger>
                  <SelectContent>
                    <div className="sticky top-0 z-10 bg-popover p-1.5">
                      <div className="relative">
                        <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
                        <Input
                          value={nameSearch}
                          onChange={(e) => setNameSearch(e.target.value)}
                          onKeyDown={(e) => e.stopPropagation()}
                          placeholder="Filter metrics…"
                          className="h-8 pl-7 font-mono text-xs"
                        />
                      </div>
                    </div>
                    {namesQuery.isPending ? (
                      <div className="p-2">
                        <Skeleton className="h-5 w-full" />
                      </div>
                    ) : filteredNames.length === 0 ? (
                      <div className="p-3 text-center text-xs text-muted-foreground">
                        {allNames.length === 0
                          ? 'No metrics ingested yet.'
                          : 'No metrics match your search.'}
                      </div>
                    ) : (
                      filteredNames.map((n) => (
                        <SelectItem
                          key={n}
                          value={n}
                          className="font-mono text-xs"
                        >
                          {n}
                        </SelectItem>
                      ))
                    )}
                  </SelectContent>
                </Select>
              </div>

              {/* Aggregation */}
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-muted-foreground">
                  Aggregation
                </label>
                <Select
                  value={aggregation}
                  onValueChange={(v) => setAggregation(v as AggKind)}
                >
                  <SelectTrigger className="w-full font-mono sm:w-[140px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {AGGREGATIONS.map((a) => (
                      <SelectItem key={a.value} value={a.value}>
                        {a.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              {/* Environment */}
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-muted-foreground">
                  Environment
                </label>
                <Select
                  value={environmentId != null ? String(environmentId) : 'all'}
                  onValueChange={(v) =>
                    setEnvironmentId(v === 'all' ? null : Number(v))
                  }
                >
                  <SelectTrigger className="w-full sm:w-[160px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All environments</SelectItem>
                    {(environments ?? []).map((env: EnvironmentResponse) => (
                      <SelectItem key={env.id} value={String(env.id)}>
                        {env.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              {/* Time range */}
              <div className="flex flex-col gap-1">
                <label className="text-xs font-medium text-muted-foreground">
                  Range
                </label>
                <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                  <Select
                    value={isCustom ? 'custom' : timeRange}
                    onValueChange={(v) => {
                      // "custom" seeds the picker with the window currently on screen.
                      if (v === 'custom') setCustomRange(fromDate, toDate)
                      else setTimeRange(v as TimeRange)
                    }}
                  >
                    <SelectTrigger className="w-full sm:w-[160px]">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {TIME_RANGES.map((r) => (
                        <SelectItem key={r.value} value={r.value}>
                          {r.label}
                        </SelectItem>
                      ))}
                      <SelectItem value="custom">Custom range…</SelectItem>
                    </SelectContent>
                  </Select>
                  {isCustom && (
                    <DateRangePicker
                      date={{ from: fromDate, to: toDate }}
                      onDateChange={(range) => {
                        if (range?.from && range?.to)
                          setCustomRange(range.from, range.to)
                      }}
                    />
                  )}
                </div>
              </div>
            </div>

            {/* Service-name filter + label-filter builder */}
            <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
              <div className="relative flex-1">
                <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={serviceName}
                  onChange={(e) => setServiceName(e.target.value)}
                  placeholder="Filter by service name…"
                  className="pl-8 font-mono text-xs"
                />
              </div>
            </div>

            {/* Label filters — keys/values autocomplete from the metric's
                observed attributes (free text still allowed). */}
            <LabelFilterBuilder
              value={labelFilters}
              onChange={setLabelFilters}
              projectId={project.id}
              metricName={metricName}
              fromIso={fromIso}
              toIso={toIso}
            />
          </div>
        </CollapsibleContent>
      </Collapsible>

      {metricName ? (
        <>
          {/* Selected-metric detail view */}
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setMetricName('')}
            className="-mb-1 gap-1.5 self-start px-2 text-xs text-muted-foreground"
          >
            <ArrowLeft className="size-3.5" />
            All metrics
          </Button>
          <div className="rounded-lg border border-border bg-card p-4">
            <MetricChart
              metricName={metricName}
              aggLabel={aggLabel}
              isPending={metricsQuery.isPending && metricName.length > 0}
              isError={metricsQuery.isError}
              errorMessage={
                metricsQuery.error instanceof Error
                  ? metricsQuery.error.message
                  : 'Failed to load metric series.'
              }
              chartData={chartDataWithBand}
              thresholds={thresholds}
              bandSeries={bandSeries}
              markers={deployMarkers}
            />
          </div>
          <MetricCorrelations
            project={project}
            metricName={metricName}
            aggregation={aggregation}
            timeRange={timeRange}
            environmentId={environmentId}
            deployCount={deployMarkers.length}
          />
          {latestHist && (
            <div className="rounded-lg border border-border bg-card p-4">
              <HistogramDistribution hist={latestHist} />
            </div>
          )}
        </>
      ) : (
        // Default view: an overview of ALL metrics, each a mini chart. Click to
        // drill into the detailed explorer above.
        <MetricsOverview
          project={project}
          names={filteredNames}
          fromIso={fromIso}
          toIso={toIso}
          bucketInterval={bucketInterval}
          aggregation={aggregation}
          onSelect={setMetricName}
          isLoadingNames={namesQuery.isPending}
          totalCount={allNames.length}
        />
      )}
    </div>
  )
}

// ── All-metrics overview ──────────────────────────────────────────────────────

const OVERVIEW_LIMIT = 24

function MetricsOverview({
  project,
  names,
  fromIso,
  toIso,
  bucketInterval,
  aggregation,
  onSelect,
  isLoadingNames,
  totalCount,
}: {
  project: ProjectResponse
  names: string[]
  fromIso: string
  toIso: string
  bucketInterval: string
  aggregation: AggKind
  onSelect: (name: string) => void
  isLoadingNames: boolean
  totalCount: number
}) {
  // One alert fetch for the whole grid (cached). Float metrics with a firing
  // rule to the top so the 24-cap can't hide what's actually broken.
  const alerts = useAlertStatus(project.id)
  const sorted = useMemo(() => {
    // alert/warn/nodata/ok rank 0–3; metrics with no rule rank last (4).
    const rank = (n: string) => {
      const s = alerts.statusFor(n, aggregation)
      return s ? statusRank(s) : 4
    }
    return [...names].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b))
  }, [names, alerts, aggregation])

  if (isLoadingNames) {
    return (
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {[0, 1, 2, 3, 4, 5].map((i) => (
          <Skeleton key={i} className="h-[190px] w-full rounded-lg" />
        ))}
      </div>
    )
  }

  if (names.length === 0) {
    return (
      <div className="rounded-lg border border-border bg-card p-4">
        <EmptyState
          icon={LineChartIcon}
          title={
            totalCount === 0 ? 'No metrics ingested yet' : 'No metrics match'
          }
          description={
            totalCount === 0
              ? 'Point an OpenTelemetry exporter at this project to start seeing metrics here.'
              : 'No metrics match your search. Clear the filter to see them all.'
          }
        />
      </div>
    )
  }

  const shown = sorted.slice(0, OVERVIEW_LIMIT)
  return (
    <div className="flex flex-col gap-3">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {shown.map((n) => (
          <MetricCard
            key={n}
            project={project}
            metricName={n}
            fromIso={fromIso}
            toIso={toIso}
            bucketInterval={bucketInterval}
            aggregation={aggregation}
            status={alerts.statusFor(n, aggregation)}
            onSelect={onSelect}
          />
        ))}
      </div>
      {names.length > shown.length && (
        <p className="text-center text-xs text-muted-foreground">
          Showing {shown.length} of {names.length} metrics — use the search in
          the Metric selector to find the rest.
        </p>
      )}
    </div>
  )
}

function MetricCard({
  project,
  metricName,
  fromIso,
  toIso,
  bucketInterval,
  aggregation,
  status,
  onSelect,
}: {
  project: ProjectResponse
  metricName: string
  fromIso: string
  toIso: string
  bucketInterval: string
  aggregation: AggKind
  status: AlertStatusLevel | null
  onSelect: (name: string) => void
}) {
  // Tone the line only for active problems; OK/no-data/unwatched stay neutral.
  const lineTone =
    status === 'alert' ? 'poor' : status === 'warn' ? 'warn' : 'primary'
  const isPercentile = aggregation.startsWith('p')
  const q = useQuery({
    ...queryMetricsOptions({
      query: {
        project_id: project.id,
        metric_name: metricName,
        start_time: fromIso,
        end_time: toIso,
        bucket_interval: bucketInterval,
        aggregation,
        limit: 500,
      },
    }),
    enabled: !!project.id,
  })

  const buckets = q.data?.data ?? []
  const isHistogram = buckets.some((b) => b.histogram_summary)
  const data = buckets.map((b) => {
    const hs = b.histogram_summary
    const value =
      isPercentile && hs && hs.bounds.length > 0
        ? histogramQuantile(
            hs.bounds,
            hs.bucket_counts,
            percentileFromAgg(aggregation),
            hs.min,
            hs.max
          )
        : (b.value ?? b.avg_value)
    return { label: formatBucketLabel(b.bucket), value }
  })
  const latest = data.length ? data[data.length - 1].value : null

  return (
    <button
      type="button"
      onClick={() => onSelect(metricName)}
      className="group flex flex-col gap-2 rounded-lg border border-border bg-card p-3 text-left transition hover:border-primary/40 hover:shadow-sm"
    >
      <div className="flex items-center justify-between gap-2">
        <span className="flex min-w-0 items-center gap-1.5">
          {status && (
            <StatusDot
              level={status}
              pulse
              title={`Alert rule: ${STATUS_META[status].label}`}
            />
          )}
          <span
            className="truncate font-mono text-xs font-medium"
            title={metricName}
          >
            {metricName}
          </span>
        </span>
        {isHistogram && (
          <Badge variant="outline" className="shrink-0 text-[10px]">
            histogram
          </Badge>
        )}
      </div>
      {q.isPending ? (
        <Skeleton className="h-[110px] w-full" />
      ) : data.length === 0 ? (
        <div className="flex h-[110px] items-center justify-center text-xs text-muted-foreground">
          No data in range
        </div>
      ) : (
        <ThresholdLineChart
          data={data}
          xKey="label"
          series={{ dataKey: 'value', label: metricName, tone: lineTone }}
          height={110}
          tooltipValueFormatter={(v) => formatMetricValue(v)}
          yTickFormatter={(v) => formatMetricValue(v)}
        />
      )}
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span className="uppercase tracking-wide text-[10px]">latest</span>
        <span className="font-mono tabular-nums">
          {latest != null ? formatMetricValue(latest) : '—'}
        </span>
      </div>
    </button>
  )
}

// ── Label filter builder ─────────────────────────────────────────────────────

function LabelFilterBuilder({
  value,
  onChange,
  projectId,
  metricName,
  fromIso,
  toIso,
}: {
  value: LabelFilter[]
  onChange: (next: LabelFilter[]) => void
  projectId: number
  /** The selected metric whose attributes drive autocomplete; '' in overview. */
  metricName: string
  fromIso: string
  toIso: string
}) {
  // Draft rows live in LOCAL state so an incomplete/empty row can exist while
  // you type. The URL (via onChange → serializeLabelFilters) only ever keeps the
  // COMPLETE pairs, so a freshly-added blank row would otherwise round-trip back
  // out of existence and the "Add" button would appear to do nothing.
  const [rows, setRows] = useState<LabelFilter[]>(value)

  // Re-seed only on an EXTERNAL change (back/forward, shared link) — i.e. when
  // the URL's complete set diverges from ours. Our own edits echo back equal, so
  // the in-progress drafts survive instead of being clobbered.
  const valueKey = serializeLabelFilters(value)
  useEffect(() => {
    if (valueKey !== serializeLabelFilters(rows)) setRows(value)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [valueKey])

  const commit = (next: LabelFilter[]) => {
    setRows(next)
    onChange(next)
  }
  const add = () => commit([...rows, { key: '', value: '' }])
  const update = (i: number, patch: Partial<LabelFilter>) =>
    commit(rows.map((f, idx) => (idx === i ? { ...f, ...patch } : f)))
  const remove = (i: number) => commit(rows.filter((_, idx) => idx !== i))

  // Discover the attribute keys present on the selected metric. Disabled in the
  // all-metrics overview (no single metric to inspect) — rows fall back to free
  // text there.
  const keysQuery = useQuery({
    ...listMetricLabelKeysOptions({
      query: {
        project_id: projectId,
        metric_name: metricName,
        start_time: fromIso,
        end_time: toIso,
      },
    }),
    enabled: !!projectId && metricName.length > 0,
  })
  const allKeys = keysQuery.data?.keys ?? []

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
          <Tag className="size-3.5" />
          Label filters
        </div>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={add}
          className="h-7 gap-1 px-2 text-xs"
        >
          <Plus className="size-3" />
          Add
        </Button>
      </div>
      {rows.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          No label filters. Add key/value pairs to narrow the series by
          attribute (e.g. <span className="font-mono">http.method = GET</span>).
        </p>
      ) : (
        <div className="flex flex-col gap-1.5">
          {rows.map((f, i) => (
            <LabelFilterRow
              // Index key is stable here — rows are only appended/removed, never
              // reordered — so the per-row value query keeps its identity.
              key={i}
              row={f}
              // Suggest only keys not already used by another row.
              availableKeys={allKeys.filter(
                (k) => k === f.key || !rows.some((r) => r.key === k)
              )}
              keysLoading={keysQuery.isFetching}
              projectId={projectId}
              metricName={metricName}
              fromIso={fromIso}
              toIso={toIso}
              onUpdate={(patch) => update(i, patch)}
              onRemove={() => remove(i)}
            />
          ))}
          <p className="text-[11px] text-muted-foreground">
            Keys and values autocomplete from the metric&apos;s observed
            attributes (last 24h). Free text is still accepted for values not in
            the recent sample. Filters are URL-persisted so the view stays
            shareable.
          </p>
        </div>
      )}
    </div>
  )
}

/** One key=value row with autocomplete on both sides. */
function LabelFilterRow({
  row,
  availableKeys,
  keysLoading,
  projectId,
  metricName,
  fromIso,
  toIso,
  onUpdate,
  onRemove,
}: {
  row: LabelFilter
  availableKeys: string[]
  keysLoading: boolean
  projectId: number
  metricName: string
  fromIso: string
  toIso: string
  onUpdate: (patch: Partial<LabelFilter>) => void
  onRemove: () => void
}) {
  // Values for THIS row's key — fetched only once a key is chosen on a metric.
  const valuesQuery = useQuery({
    ...listMetricLabelValuesOptions({
      query: {
        project_id: projectId,
        metric_name: metricName,
        label_key: row.key,
        start_time: fromIso,
        end_time: toIso,
      },
    }),
    enabled: !!projectId && metricName.length > 0 && row.key.trim().length > 0,
  })

  return (
    <div className="flex items-center gap-1.5">
      <SuggestCombobox
        value={row.key}
        options={availableKeys}
        loading={keysLoading}
        placeholder="label.key"
        searchPlaceholder="Search keys…"
        ariaLabel="Label key"
        widthClass="w-full sm:w-[200px]"
        // Changing the key invalidates the previously chosen value.
        onChange={(next) => onUpdate({ key: next, value: '' })}
      />
      <span className="text-muted-foreground">=</span>
      <SuggestCombobox
        value={row.value}
        options={valuesQuery.data?.values ?? []}
        loading={valuesQuery.isFetching}
        placeholder="value"
        searchPlaceholder="Search values…"
        ariaLabel="Label value"
        widthClass="flex-1"
        disabled={row.key.trim().length === 0}
        onChange={(next) => onUpdate({ value: next })}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        onClick={onRemove}
        className="h-8 w-8 shrink-0"
        aria-label="Remove label filter"
      >
        <X className="size-3.5" />
      </Button>
    </div>
  )
}

/**
 * A combobox that suggests discovered options but still accepts free text — the
 * sampled discovery may miss a rare key/value, so typing anything commits it via
 * the "Use …" affordance (or Enter).
 */
function SuggestCombobox({
  value,
  options,
  loading,
  placeholder,
  searchPlaceholder,
  ariaLabel,
  widthClass,
  disabled,
  onChange,
}: {
  value: string
  options: string[]
  loading?: boolean
  placeholder: string
  searchPlaceholder: string
  ariaLabel: string
  widthClass?: string
  disabled?: boolean
  onChange: (next: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const typed = query.trim()
  const hasExactOption = options.includes(typed)

  const choose = (next: string) => {
    onChange(next)
    setQuery('')
    setOpen(false)
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-label={ariaLabel}
          disabled={disabled}
          className={[
            'h-8 justify-between gap-1.5 px-2.5 font-mono text-xs font-normal',
            widthClass ?? '',
            value ? '' : 'text-muted-foreground',
          ].join(' ')}
        >
          <span className="truncate">{value || placeholder}</span>
          <ChevronsUpDown className="size-3.5 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[240px] p-0">
        <Command>
          <CommandInput
            value={query}
            onValueChange={setQuery}
            placeholder={searchPlaceholder}
            className="text-xs"
          />
          <CommandList className="max-h-[240px]">
            {loading ? (
              <div className="py-4 text-center text-xs text-muted-foreground">
                Loading…
              </div>
            ) : (
              <>
                {typed && !hasExactOption && (
                  <CommandItem value={typed} onSelect={() => choose(typed)}>
                    <Check className="size-3.5 shrink-0 opacity-0" />
                    <span className="truncate font-mono text-xs">
                      Use “{typed}”
                    </span>
                  </CommandItem>
                )}
                <CommandEmpty>No matches.</CommandEmpty>
                {options.map((o) => (
                  <CommandItem
                    key={o}
                    value={o}
                    onSelect={() => choose(o)}
                    className="gap-2"
                  >
                    <Check
                      className={[
                        'size-3.5 shrink-0',
                        o === value ? 'opacity-100' : 'opacity-0',
                      ].join(' ')}
                    />
                    <span className="truncate font-mono text-xs">{o}</span>
                  </CommandItem>
                ))}
              </>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

// ── Chart + states ───────────────────────────────────────────────────────────

interface ChartPoint {
  bucket: string
  value: number
  avg_value: number
  min_value: number
  max_value: number
  count: number
}

function MetricChart({
  metricName,
  aggLabel,
  isPending,
  isError,
  errorMessage,
  chartData,
  thresholds,
  bandSeries,
  markers,
}: {
  metricName: string
  aggLabel: string
  isPending: boolean
  isError: boolean
  errorMessage: string
  chartData: ChartPoint[]
  thresholds?: ThresholdBand[]
  bandSeries?: ThresholdBandSeries
  markers?: ThresholdMarker[]
}) {
  if (!metricName) {
    return (
      <EmptyState
        icon={LineChartIcon}
        title="Pick a metric to chart"
        description="Choose a metric name from the selector above to render its time series. Series are bucketed server-side over the selected range."
      />
    )
  }

  if (isPending) {
    return <Skeleton className="h-[300px] w-full" />
  }

  if (isError) {
    return (
      <div className="flex h-[300px] flex-col items-center justify-center gap-2 text-center">
        <p className="text-sm font-medium text-rose-500">
          Failed to load metric series
        </p>
        <p className="text-xs text-muted-foreground">{errorMessage}</p>
      </div>
    )
  }

  if (chartData.length === 0) {
    return (
      <EmptyState
        icon={LineChartIcon}
        title="No data in range"
        description="This metric has no samples in the selected time range. Try widening the range or clearing filters."
      />
    )
  }

  // Format bucket timestamps for the X axis. Buckets arrive as ISO strings
  // from the backend; render a compact HH:mm (or date for wide ranges).
  const xData = chartData.map((p) => ({
    ...p,
    label: formatBucketLabel(p.bucket),
  }))

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <Badge variant="secondary" className="font-mono text-xs">
          {metricName}
        </Badge>
        <span className="text-xs text-muted-foreground">{aggLabel}</span>
      </div>
      <ThresholdLineChart
        data={xData}
        xKey="label"
        series={{ dataKey: 'value', label: aggLabel, tone: 'primary' }}
        thresholds={thresholds}
        bandSeries={bandSeries}
        markers={markers}
        height={320}
        tooltipValueFormatter={(v) => formatMetricValue(v)}
        yTickFormatter={(v) => formatMetricValue(v)}
      />
    </div>
  )
}

function formatBucketLabel(bucket: string): string {
  const d = new Date(bucket)
  if (Number.isNaN(d.getTime())) return bucket
  return format(d, 'MMM d HH:mm')
}

function formatMetricValue(v: number): string {
  if (!Number.isFinite(v)) return '—'
  const abs = Math.abs(v)
  if (abs >= 1_000_000) return `${(v / 1_000_000).toFixed(2)}M`
  if (abs >= 1_000) return `${(v / 1_000).toFixed(2)}k`
  if (abs >= 1) return v.toFixed(2)
  return v.toPrecision(3)
}

// ── Histogram helpers ─────────────────────────────────────────────────────────

function percentileFromAgg(agg: AggKind): number {
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
function histogramQuantile(
  bounds: number[],
  counts: number[],
  q: number,
  min?: number | null,
  max?: number | null
): number {
  const total = counts.reduce((a, c) => a + c, 0)
  if (total === 0) return 0
  const target = q * total
  // Bound the first/last populated buckets to the recorded [min, max] so a
  // single over-wide bucket (e.g. sub-ms samples in a 0–5ms bucket) doesn't
  // spread quantiles across the whole bucket width (p99≈5 vs a true max ≈0.5).
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

// ── Histogram distribution panel ──────────────────────────────────────────────

function HistogramDistribution({ hist }: { hist: HistogramSummary }) {
  const total = hist.count
  const maxCount = Math.max(1, ...hist.bucket_counts)
  const mean = total > 0 ? hist.sum / total : 0
  const stats: { label: string; value: number }[] = [
    { label: 'Count', value: total },
    { label: 'Mean', value: mean },
    {
      label: 'p50',
      value: histogramQuantile(
        hist.bounds,
        hist.bucket_counts,
        0.5,
        hist.min,
        hist.max
      ),
    },
    {
      label: 'p90',
      value: histogramQuantile(
        hist.bounds,
        hist.bucket_counts,
        0.9,
        hist.min,
        hist.max
      ),
    },
    {
      label: 'p95',
      value: histogramQuantile(
        hist.bounds,
        hist.bucket_counts,
        0.95,
        hist.min,
        hist.max
      ),
    },
    {
      label: 'p99',
      value: histogramQuantile(
        hist.bounds,
        hist.bucket_counts,
        0.99,
        hist.min,
        hist.max
      ),
    },
  ]
  // One row per bucket: label is the [lower, upper) range; the last is +Inf.
  const rows = hist.bucket_counts.map((c, i) => {
    const lower = i === 0 ? 0 : hist.bounds[i - 1]
    const upper = i < hist.bounds.length ? hist.bounds[i] : Infinity
    const range =
      upper === Infinity
        ? `≥ ${formatMetricValue(lower)}`
        : `${formatMetricValue(lower)} – ${formatMetricValue(upper)}`
    return { range, count: c }
  })

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <BarChart3 className="size-4 text-muted-foreground" />
        <h2 className="text-sm font-semibold">Distribution</h2>
        <span className="text-xs text-muted-foreground">latest bucket</span>
      </div>
      <div className="grid grid-cols-3 gap-2 sm:grid-cols-6">
        {stats.map((s) => (
          <div
            key={s.label}
            className="flex flex-col gap-0.5 rounded-md border border-border/60 bg-muted/30 p-2"
          >
            <span className="text-[11px] uppercase tracking-wide text-muted-foreground">
              {s.label}
            </span>
            <span className="font-mono text-sm font-medium">
              {formatMetricValue(s.value)}
            </span>
          </div>
        ))}
      </div>
      <div className="flex flex-col gap-1">
        {rows.map((r, i) => (
          <div key={i} className="flex items-center gap-2 text-xs">
            <span className="w-36 shrink-0 text-right font-mono text-muted-foreground">
              {r.range}
            </span>
            <div className="relative h-4 flex-1 overflow-hidden rounded bg-muted/40">
              <div
                className="h-full rounded bg-primary/70"
                style={{ width: `${(r.count / maxCount) * 100}%` }}
              />
            </div>
            <span className="w-12 shrink-0 text-right font-mono tabular-nums text-muted-foreground">
              {r.count}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}
