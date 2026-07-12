/**
 * ProxyMetrics — charts for proxy hot-path traffic.
 *
 * Route: /proxy
 *
 * Two data sources, switched by the project/environment filter:
 *   - "All projects" (default): process-wide proxy.* node metrics on the
 *     control-plane node (id 0). These have no project dimension by design.
 *   - Project/environment filtered: proxy-log-derived time buckets from
 *     GET /proxy-logs/stats/time-buckets (request/error counts, avg latency,
 *     bandwidth). Logs carry no percentiles, so the latency-percentile panel
 *     is replaced by a bandwidth panel in filtered mode.
 *
 * All data comes from generated SDK bindings — never hand-rolled fetch.
 */

import {
  getEnvironmentsOptions,
  getProjectsHealthOptions,
  getProjectsOptions,
  getTimeBucketStatsOptions,
  nodeMetricsGetRangeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { TimeBucketStats } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { TOOLTIP_CONTENT_STYLE, TOOLTIP_LABEL_STYLE } from '@/lib/chart-tooltip'
import { useQueries, useQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useState } from 'react'
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** The control-plane node always has id 0. */
const CONTROL_PLANE_NODE_ID = 0

const RANGE_OPTIONS = [
  { value: '1h', label: '1h' },
  { value: '6h', label: '6h' },
  { value: '24h', label: '24h' },
  { value: '7d', label: '7d' },
] as const

type RangeValue = (typeof RANGE_OPTIONS)[number]['value']

const RANGE_SECONDS: Record<RangeValue, number> = {
  '1h': 3_600,
  '6h': 21_600,
  '24h': 86_400,
  '7d': 604_800,
}

/** Bucket steps matching the node-metric endpoint's per-range resolution. */
const RANGE_BUCKET_INTERVAL: Record<RangeValue, string> = {
  '1h': '1 minute',
  '6h': '5 minutes',
  '24h': '15 minutes',
  '7d': '1 hour',
}

/** One line in a chart panel: data key + display label + stroke color. */
type SeriesDef = {
  dataKey: string
  label: string
  color: string
}

const formatCount = (v: number) =>
  v >= 1_000_000
    ? `${(v / 1_000_000).toFixed(1)}M`
    : v >= 1_000
      ? `${(v / 1_000).toFixed(1)}k`
      : `${Math.round(v)}`

const formatPercent = (v: number) => `${v.toFixed(2)}%`

const formatMs = (v: number) =>
  v >= 1_000 ? `${(v / 1_000).toFixed(2)}s` : `${v.toFixed(1)}ms`

const formatBytesShort = (v: number) => {
  if (v >= 1_073_741_824) return `${(v / 1_073_741_824).toFixed(2)} GB`
  if (v >= 1_048_576) return `${(v / 1_048_576).toFixed(1)} MB`
  if (v >= 1_024) return `${(v / 1_024).toFixed(1)} KB`
  return `${Math.round(v)} B`
}

// ---------------------------------------------------------------------------
// Node-metric panels (unfiltered "All projects" view)
// ---------------------------------------------------------------------------

type NodePanelDef = {
  title: string
  description: string
  series: (SeriesDef & { metric: string })[]
  valueFormatter: (v: number) => string
}

const NODE_PANELS: NodePanelDef[] = [
  {
    title: 'Requests by status class',
    description: 'Per-interval request count, split by response status class',
    series: [
      {
        metric: 'proxy.requests',
        dataKey: 'proxy.requests',
        label: 'Total',
        color: '#2563eb',
      },
      {
        metric: 'proxy.requests_2xx',
        dataKey: 'proxy.requests_2xx',
        label: '2xx',
        color: '#16a34a',
      },
      {
        metric: 'proxy.requests_4xx',
        dataKey: 'proxy.requests_4xx',
        label: '4xx',
        color: '#d97706',
      },
      {
        metric: 'proxy.requests_5xx',
        dataKey: 'proxy.requests_5xx',
        label: '5xx',
        color: '#dc2626',
      },
    ],
    valueFormatter: formatCount,
  },
  {
    title: 'Requests by destination',
    description:
      'Per-interval request count, split by destination: project routes, console fallback, or handled by the proxy itself',
    series: [
      {
        metric: 'proxy.requests_project',
        dataKey: 'proxy.requests_project',
        label: 'Project',
        color: '#2563eb',
      },
      {
        metric: 'proxy.requests_console',
        dataKey: 'proxy.requests_console',
        label: 'Console',
        color: '#7c3aed',
      },
      {
        metric: 'proxy.requests_other',
        dataKey: 'proxy.requests_other',
        label: 'Other',
        color: '#6b7280',
      },
    ],
    valueFormatter: formatCount,
  },
  {
    title: 'Error rate',
    description: 'Percentage of requests answered with a 5xx status',
    series: [
      {
        metric: 'proxy.error_rate_percent',
        dataKey: 'proxy.error_rate_percent',
        label: 'Error rate',
        color: '#dc2626',
      },
    ],
    valueFormatter: formatPercent,
  },
  {
    title: 'Latency percentiles',
    description: 'Request duration p50 / p95 / p99',
    series: [
      {
        metric: 'proxy.request_duration_p50_ms',
        dataKey: 'proxy.request_duration_p50_ms',
        label: 'p50',
        color: '#16a34a',
      },
      {
        metric: 'proxy.request_duration_p95_ms',
        dataKey: 'proxy.request_duration_p95_ms',
        label: 'p95',
        color: '#d97706',
      },
      {
        metric: 'proxy.request_duration_p99_ms',
        dataKey: 'proxy.request_duration_p99_ms',
        label: 'p99',
        color: '#dc2626',
      },
    ],
    valueFormatter: formatMs,
  },
  {
    title: 'Backend latency percentiles',
    description:
      'Upstream duration p50 / p95 / p99 (proxied requests only: connect + processing + TTFB)',
    series: [
      {
        metric: 'proxy.upstream_duration_p50_ms',
        dataKey: 'proxy.upstream_duration_p50_ms',
        label: 'p50',
        color: '#16a34a',
      },
      {
        metric: 'proxy.upstream_duration_p95_ms',
        dataKey: 'proxy.upstream_duration_p95_ms',
        label: 'p95',
        color: '#d97706',
      },
      {
        metric: 'proxy.upstream_duration_p99_ms',
        dataKey: 'proxy.upstream_duration_p99_ms',
        label: 'p99',
        color: '#dc2626',
      },
    ],
    valueFormatter: formatMs,
  },
  {
    title: 'Latency breakdown',
    description:
      'Mean request duration per interval, split into backend time and proxy overhead (proxied requests only for the split)',
    series: [
      {
        metric: 'proxy.request_duration_avg_ms',
        dataKey: 'proxy.request_duration_avg_ms',
        label: 'Total avg',
        color: '#2563eb',
      },
      {
        metric: 'proxy.upstream_duration_avg_ms',
        dataKey: 'proxy.upstream_duration_avg_ms',
        label: 'Backend avg',
        color: '#d97706',
      },
      {
        metric: 'proxy.self_duration_avg_ms',
        dataKey: 'proxy.self_duration_avg_ms',
        label: 'Proxy avg',
        color: '#16a34a',
      },
    ],
    valueFormatter: formatMs,
  },
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Active filter selection. Null project means "All projects" (node metrics). */
type ProxyFilter = {
  projectId: number | null
  environmentId: number | null
}

/** Whether an error is the endpoint's 503 "metrics store not available". */
function isMetricsUnavailable(err: unknown): boolean {
  const problem = err as { status?: number; detail?: string; title?: string }
  if (problem?.status === 503) return true
  const msg = `${problem?.detail ?? ''} ${problem?.title ?? ''}`.toLowerCase()
  return msg.includes('not available') || msg.includes('unavailable')
}

/** Time-axis label — include the date on multi-day ranges. */
function formatTimeLabel(iso: string, range: RangeValue): string {
  const d = new Date(iso)
  if (range === '7d') {
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
  }
  return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

/** Shared query options for one proxy metric series on the CP node. */
function proxySeriesQuery(metric: string, range: RangeValue) {
  return {
    ...nodeMetricsGetRangeOptions({
      path: { id: CONTROL_PLANE_NODE_ID },
      query: { metric, range },
    }),
    staleTime: 15_000,
    refetchInterval: 30_000,
    retry: 1,
  }
}

/** Memoized window bounds for the selected range (stable query keys). */
function useWindowBounds(range: RangeValue) {
  return useMemo(() => {
    const end = new Date()
    const start = new Date(end.getTime() - RANGE_SECONDS[range] * 1000)
    return {
      startIso: start.toISOString(),
      endIso: end.toISOString(),
    }
  }, [range])
}

/**
 * Proxy-log time buckets for the filtered view. Identical options across the
 * stat cards and every chart panel, so React Query dedupes to one request.
 */
function useBucketStats(range: RangeValue, filter: ProxyFilter) {
  const { startIso, endIso } = useWindowBounds(range)
  return useQuery({
    ...getTimeBucketStatsOptions({
      query: {
        start_time: startIso,
        end_time: endIso,
        bucket_interval: RANGE_BUCKET_INTERVAL[range],
        project_id: filter.projectId ?? undefined,
        environment_id: filter.environmentId ?? undefined,
      },
    }),
    enabled: filter.projectId != null,
    staleTime: 15_000,
    refetchInterval: 30_000,
  })
}

// ---------------------------------------------------------------------------
// Shared presentational chart panel
// ---------------------------------------------------------------------------

type ChartPanelProps = {
  title: string
  description: string
  series: SeriesDef[]
  data: Record<string, string | number | null>[]
  valueFormatter: (v: number) => string
  isPending: boolean
  errorText?: string | null
  emptyText: string
}

function ChartPanel({
  title,
  description,
  series,
  data,
  valueFormatter,
  isPending,
  errorText,
  emptyText,
}: ChartPanelProps) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-base">{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>
        {isPending ? (
          <Skeleton className="h-[220px] w-full" />
        ) : errorText ? (
          <div className="flex h-[220px] items-center justify-center px-6 text-center text-sm text-muted-foreground">
            {errorText}
          </div>
        ) : data.length === 0 ? (
          <div className="flex h-[220px] items-center justify-center px-6 text-center text-sm text-muted-foreground">
            {emptyText}
          </div>
        ) : (
          <div className="h-[220px]">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart
                data={data}
                margin={{ top: 4, right: 24, left: 0, bottom: 0 }}
              >
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="rgba(128,128,128,0.15)"
                  vertical={false}
                />
                <XAxis
                  dataKey="label"
                  tick={{ fontSize: 10, fill: 'rgba(156,163,175,0.9)' }}
                  tickLine={false}
                  axisLine={false}
                  interval="preserveStartEnd"
                />
                <YAxis
                  tick={{ fontSize: 10, fill: 'rgba(156,163,175,0.9)' }}
                  tickLine={false}
                  axisLine={false}
                  width={60}
                  tickFormatter={valueFormatter}
                />
                <Tooltip
                  wrapperStyle={{ zIndex: 50 }}
                  allowEscapeViewBox={{ x: true, y: true }}
                  contentStyle={TOOLTIP_CONTENT_STYLE}
                  labelStyle={TOOLTIP_LABEL_STYLE}
                  cursor={{ stroke: 'rgba(128,128,128,0.3)', strokeWidth: 1 }}
                  formatter={(v, name) => [valueFormatter(Number(v)), name]}
                />
                {series.map((s) => (
                  <Line
                    key={s.dataKey}
                    type="monotone"
                    dataKey={s.dataKey}
                    name={s.label}
                    dot={false}
                    strokeWidth={2}
                    stroke={s.color}
                    connectNulls
                    isAnimationActive={false}
                  />
                ))}
              </LineChart>
            </ResponsiveContainer>
          </div>
        )}
        {series.length > 1 && (
          <div className="mt-2 flex flex-wrap items-center gap-3">
            {series.map((s) => (
              <span
                key={s.dataKey}
                className="flex items-center gap-1.5 text-xs text-muted-foreground"
              >
                <span
                  className="inline-block h-2 w-2 rounded-full"
                  style={{ backgroundColor: s.color }}
                />
                {s.label}
              </span>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Node-metric panel (unfiltered) — one query per series, merged on timestamp
// ---------------------------------------------------------------------------

function NodeMetricPanel({
  panel,
  range,
}: {
  panel: NodePanelDef
  range: RangeValue
}) {
  const results = useQueries({
    queries: panel.series.map((s) => proxySeriesQuery(s.metric, range)),
  })

  const isPending = results.some((r) => r.isPending)
  const errors = results.filter((r) => r.isError).map((r) => r.error)
  const allFailed = errors.length === results.length && errors.length > 0
  const errorText = allFailed
    ? errors.every(isMetricsUnavailable)
      ? 'Metric collection is not enabled on this server.'
      : 'Failed to load proxy metrics'
    : null

  // Merge per-series point arrays into one row per timestamp. Recomputed per
  // render — a few hundred points at most, not worth an unstable memo dep.
  const rows = new Map<string, Record<string, string | number | null>>()
  results.forEach((r, i) => {
    const key = panel.series[i].metric
    for (const p of r.data ?? []) {
      const row = rows.get(p.time) ?? {
        time: p.time,
        label: formatTimeLabel(p.time, range),
      }
      row[key] = p.value
      rows.set(p.time, row)
    }
  })
  const chartData = [...rows.values()].sort((a, b) =>
    String(a.time).localeCompare(String(b.time))
  )

  return (
    <ChartPanel
      title={panel.title}
      description={panel.description}
      series={panel.series}
      data={chartData}
      valueFormatter={panel.valueFormatter}
      isPending={isPending}
      errorText={errorText}
      emptyText="No proxy metrics yet — data appears within a minute of traffic"
    />
  )
}

// ---------------------------------------------------------------------------
// Filtered charts (proxy-log time buckets)
// ---------------------------------------------------------------------------

function bucketChartRows(stats: TimeBucketStats[], range: RangeValue) {
  return stats.map((b) => ({
    time: b.bucket,
    label: formatTimeLabel(b.bucket, range),
    request_count: b.request_count,
    error_count: b.error_count,
    error_rate:
      b.request_count > 0 ? (b.error_count / b.request_count) * 100 : 0,
    avg_response_time_ms: b.avg_response_time_ms,
    total_request_bytes: b.total_request_bytes,
    total_response_bytes: b.total_response_bytes,
  }))
}

function FilteredCharts({
  range,
  filter,
}: {
  range: RangeValue
  filter: ProxyFilter
}) {
  const q = useBucketStats(range, filter)
  const stats = q.data?.stats ?? []
  const data = bucketChartRows(stats, range)

  const shared = {
    data,
    isPending: q.isPending,
    errorText: q.isError ? 'Failed to load proxy log statistics' : null,
    emptyText: 'No proxy logs for this selection in the window',
  }

  return (
    <div className="space-y-2">
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        <ChartPanel
          title="Requests"
          description="Requests and errors (status ≥ 400) per interval, from proxy logs"
          series={[
            { dataKey: 'request_count', label: 'Requests', color: '#2563eb' },
            { dataKey: 'error_count', label: 'Errors', color: '#dc2626' },
          ]}
          valueFormatter={formatCount}
          {...shared}
        />
        <ChartPanel
          title="Error rate"
          description="Errors (status ≥ 400) as a share of requests per interval"
          series={[
            { dataKey: 'error_rate', label: 'Error rate', color: '#dc2626' },
          ]}
          valueFormatter={formatPercent}
          {...shared}
        />
        <ChartPanel
          title="Average duration"
          description="Mean response time per interval, from proxy logs"
          series={[
            {
              dataKey: 'avg_response_time_ms',
              label: 'avg',
              color: '#2563eb',
            },
          ]}
          valueFormatter={formatMs}
          {...shared}
        />
        <ChartPanel
          title="Bandwidth"
          description="Request and response bytes per interval"
          series={[
            {
              dataKey: 'total_request_bytes',
              label: 'Request bytes',
              color: '#16a34a',
            },
            {
              dataKey: 'total_response_bytes',
              label: 'Response bytes',
              color: '#2563eb',
            },
          ]}
          valueFormatter={formatBytesShort}
          {...shared}
        />
      </div>
      <p className="text-xs text-muted-foreground">
        Percentile latency is only available for all traffic.
      </p>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Summary stat cards
// ---------------------------------------------------------------------------

type StatCardProps = {
  title: string
  value: string | null
  isPending: boolean
  /** Optional muted one-liner under the value (e.g. a traffic split). */
  sub?: string | null
}

function StatCard({ title, value, isPending, sub }: StatCardProps) {
  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {title}
        </CardTitle>
      </CardHeader>
      <CardContent>
        {isPending ? (
          <Skeleton className="h-8 w-24" />
        ) : (
          <>
            <div className="text-2xl font-semibold tracking-tight tabular-nums">
              {value ?? '—'}
            </div>
            {sub && (
              <p className="mt-1 text-[11px] text-muted-foreground">{sub}</p>
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

/** Unfiltered stats — computed from the process-wide node metric series. */
function NodeSummaryStats({ range }: { range: RangeValue }) {
  // Same query keys the chart panels use — React Query dedupes the fetches.
  const [requests, errors5xx, p95, destProject, destConsole, destOther] =
    useQueries({
      queries: [
        proxySeriesQuery('proxy.requests', range),
        proxySeriesQuery('proxy.requests_5xx', range),
        proxySeriesQuery('proxy.request_duration_p95_ms', range),
        // Destination split — same options the "Requests by destination"
        // panel uses, so React Query dedupes the fetches.
        proxySeriesQuery('proxy.requests_project', range),
        proxySeriesQuery('proxy.requests_console', range),
        proxySeriesQuery('proxy.requests_other', range),
      ],
    })

  const totalRequests = (requests.data ?? []).reduce(
    (acc, p) => acc + (p.value ?? 0),
    0
  )
  const total5xx = (errors5xx.data ?? []).reduce(
    (acc, p) => acc + (p.value ?? 0),
    0
  )
  const latestP95 = [...(p95.data ?? [])]
    .reverse()
    .find((p) => p.value != null)?.value

  const hasRequests = !requests.isPending && !requests.isError

  // Muted destination split under Total requests — only when we have traffic
  // and all three destination series loaded (they always sum to
  // proxy.requests on the backend).
  const sumSeries = (q: typeof destProject | undefined) =>
    (q?.data ?? []).reduce((acc, p) => acc + (p.value ?? 0), 0)
  const destsReady = [destProject, destConsole, destOther].every(
    (q) => !q.isPending && !q.isError
  )
  const destSplit =
    hasRequests && destsReady && totalRequests > 0
      ? (() => {
          const pct = (n: number) => Math.round((n / totalRequests) * 100)
          return `${pct(sumSeries(destProject))}% project · ${pct(sumSeries(destConsole))}% console · ${pct(sumSeries(destOther))}% other`
        })()
      : null

  return (
    <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
      <StatCard
        title="Requests/s"
        isPending={requests.isPending}
        value={
          hasRequests
            ? `${(totalRequests / RANGE_SECONDS[range]).toFixed(2)}/s`
            : null
        }
      />
      <StatCard
        title="Total requests"
        isPending={requests.isPending}
        value={hasRequests ? formatCount(totalRequests) : null}
        sub={destSplit}
      />
      <StatCard
        title="Error rate"
        isPending={requests.isPending || errors5xx.isPending}
        value={
          hasRequests && !errors5xx.isError && totalRequests > 0
            ? formatPercent((total5xx / totalRequests) * 100)
            : null
        }
      />
      <StatCard
        title="p95 latency"
        isPending={p95.isPending}
        value={latestP95 != null ? formatMs(latestP95) : null}
      />
    </div>
  )
}

/** Filtered stats — computed from the proxy-log time buckets. */
function FilteredSummaryStats({
  range,
  filter,
}: {
  range: RangeValue
  filter: ProxyFilter
}) {
  const q = useBucketStats(range, filter)
  const stats = q.data?.stats ?? []

  const totalRequests = stats.reduce((acc, b) => acc + b.request_count, 0)
  const totalErrors = stats.reduce((acc, b) => acc + b.error_count, 0)
  // Weighted average of per-bucket means by request count.
  const weightedAvg =
    totalRequests > 0
      ? stats.reduce(
          (acc, b) => acc + b.avg_response_time_ms * b.request_count,
          0
        ) / totalRequests
      : null

  const ok = !q.isPending && !q.isError

  return (
    <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
      <StatCard
        title="Requests/s"
        isPending={q.isPending}
        value={
          ok ? `${(totalRequests / RANGE_SECONDS[range]).toFixed(2)}/s` : null
        }
      />
      <StatCard
        title="Total requests"
        isPending={q.isPending}
        value={ok ? formatCount(totalRequests) : null}
      />
      <StatCard
        title="4xx+5xx rate"
        isPending={q.isPending}
        value={
          ok && totalRequests > 0
            ? formatPercent((totalErrors / totalRequests) * 100)
            : null
        }
      />
      <StatCard
        title="Avg latency"
        isPending={q.isPending}
        value={ok && weightedAvg != null ? formatMs(weightedAvg) : null}
      />
    </div>
  )
}

// ---------------------------------------------------------------------------
// Filter bar (project + environment selects)
// ---------------------------------------------------------------------------

const ALL_SENTINEL = 'all'

function FilterBar({
  filter,
  onChange,
}: {
  filter: ProxyFilter
  onChange: (f: ProxyFilter) => void
}) {
  const projectsQ = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    staleTime: 60_000,
  })
  const environmentsQ = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: filter.projectId ?? 0 },
    }),
    enabled: filter.projectId != null,
    staleTime: 60_000,
  })

  const projects = projectsQ.data?.projects ?? []
  const environments = environmentsQ.data ?? []

  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
      <Select
        value={
          filter.projectId != null ? String(filter.projectId) : ALL_SENTINEL
        }
        onValueChange={(v) =>
          onChange({
            projectId: v === ALL_SENTINEL ? null : Number(v),
            environmentId: null,
          })
        }
      >
        <SelectTrigger className="w-full sm:w-[200px]">
          <SelectValue placeholder="All projects" />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ALL_SENTINEL}>All projects</SelectItem>
          {projects.map((p) => (
            <SelectItem key={p.id} value={String(p.id)}>
              {p.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {filter.projectId != null && (
        <Select
          value={
            filter.environmentId != null
              ? String(filter.environmentId)
              : ALL_SENTINEL
          }
          onValueChange={(v) =>
            onChange({
              ...filter,
              environmentId: v === ALL_SENTINEL ? null : Number(v),
            })
          }
        >
          <SelectTrigger className="w-full sm:w-[180px]">
            <SelectValue placeholder="All environments" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL_SENTINEL}>All environments</SelectItem>
            {environments.map((e) => (
              <SelectItem key={e.id} value={String(e.id)}>
                {e.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Traffic by project (proxy-log request stats per project)
// ---------------------------------------------------------------------------

/** One table row: project identity joined with its proxy-log health stats. */
type ProjectTrafficRow = {
  project_id: number
  project_name: string
  total_requests: number
  total_errors: number
  error_rate: number
  avg_response_time_ms: number
  status: string
}

type TrafficSortKey =
  | 'project_name'
  | 'total_requests'
  | 'total_errors'
  | 'error_rate'
  | 'avg_response_time_ms'
  | 'status'

const TRAFFIC_COLUMNS: {
  key: TrafficSortKey
  label: string
  numeric: boolean
  secondary: boolean
}[] = [
  { key: 'project_name', label: 'Project', numeric: false, secondary: false },
  { key: 'total_requests', label: 'Requests', numeric: true, secondary: false },
  { key: 'total_errors', label: 'Errors', numeric: true, secondary: true },
  { key: 'error_rate', label: 'Error rate', numeric: true, secondary: false },
  {
    key: 'avg_response_time_ms',
    label: 'Avg latency',
    numeric: true,
    secondary: true,
  },
  { key: 'status', label: 'Status', numeric: false, secondary: false },
]

const STATUS_STYLES: Record<string, { dot: string; text: string }> = {
  healthy: { dot: 'bg-emerald-500', text: 'text-emerald-600' },
  degraded: { dot: 'bg-amber-500', text: 'text-amber-600' },
  down: { dot: 'bg-red-500', text: 'text-red-600' },
  unknown: { dot: 'bg-muted-foreground/40', text: 'text-muted-foreground' },
}

function StatusBadge({ status }: { status: string }) {
  const style = STATUS_STYLES[status] ?? STATUS_STYLES.unknown
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs ${style.text}`}>
      <span className={`inline-block h-2 w-2 rounded-full ${style.dot}`} />
      {status}
    </span>
  )
}

function TrafficByProject({
  range,
  filter,
}: {
  range: RangeValue
  filter: ProxyFilter
}) {
  const [sortKey, setSortKey] = useState<TrafficSortKey>('total_requests')
  const [sortDesc, setSortDesc] = useState(true)
  const { startIso, endIso } = useWindowBounds(range)

  // Same options FilterBar uses — React Query dedupes the fetch.
  const projectsQ = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    staleTime: 60_000,
  })
  const allProjects = projectsQ.data?.projects ?? []
  const projects =
    filter.projectId != null
      ? allProjects.filter((p) => p.id === filter.projectId)
      : allProjects

  const idsParam = projects.map((p) => p.id).join(',')
  const healthQ = useQuery({
    ...getProjectsHealthOptions({
      query: {
        project_ids: idsParam,
        start_time: startIso,
        end_time: endIso,
      },
    }),
    enabled: projects.length > 0,
    staleTime: 30_000,
    refetchInterval: 60_000,
  })

  const health = healthQ.data?.projects ?? {}
  const rows: ProjectTrafficRow[] = projects.map((p) => {
    const h = health[String(p.id)]
    return {
      project_id: p.id,
      project_name: p.name,
      total_requests: h?.total_requests ?? 0,
      total_errors: h?.total_errors ?? 0,
      error_rate: h?.error_rate ?? 0,
      avg_response_time_ms: h?.avg_response_time_ms ?? 0,
      status: h?.status ?? 'unknown',
    }
  })

  const sorted = [...rows].sort((a, b) => {
    const av = a[sortKey]
    const bv = b[sortKey]
    const cmp =
      typeof av === 'number' && typeof bv === 'number'
        ? av - bv
        : String(av ?? '').localeCompare(String(bv ?? ''))
    return sortDesc ? -cmp : cmp
  })

  const onSort = (key: TrafficSortKey) => {
    if (key === sortKey) {
      setSortDesc((d) => !d)
    } else {
      setSortKey(key)
      setSortDesc(true)
    }
  }

  const isPending =
    projectsQ.isPending || (projects.length > 0 && healthQ.isPending)

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-base">Traffic by project</CardTitle>
        <CardDescription>
          Requests per project from proxy logs over the selected window
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
          </div>
        ) : projectsQ.isError || healthQ.isError ? (
          <div className="py-8 text-center text-sm text-rose-500">
            Failed to load project traffic
          </div>
        ) : sorted.length === 0 ? (
          <div className="py-8 text-center text-sm text-muted-foreground">
            No projects yet
          </div>
        ) : (
          <div className="overflow-x-auto">
            <Table className="min-w-[480px]">
              <TableHeader>
                <TableRow>
                  {TRAFFIC_COLUMNS.map((col) => (
                    <TableHead
                      key={col.key}
                      className={
                        (col.numeric ? 'text-right ' : '') +
                        (col.secondary ? 'hidden md:table-cell' : '')
                      }
                    >
                      <button
                        type="button"
                        onClick={() => onSort(col.key)}
                        className="inline-flex items-center gap-1 hover:text-foreground"
                      >
                        {col.label}
                        {sortKey === col.key && (
                          <span aria-hidden>{sortDesc ? '↓' : '↑'}</span>
                        )}
                      </button>
                    </TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {sorted.map((row) => (
                  <TableRow
                    key={row.project_id}
                    className={
                      row.total_requests === 0 ? 'text-muted-foreground' : ''
                    }
                  >
                    <TableCell className="font-medium">
                      {row.project_name}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {formatCount(row.total_requests)}
                    </TableCell>
                    <TableCell className="hidden text-right tabular-nums md:table-cell">
                      {formatCount(row.total_errors)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {formatPercent(row.error_rate)}
                    </TableCell>
                    <TableCell className="hidden text-right tabular-nums md:table-cell">
                      {formatMs(row.avg_response_time_ms)}
                    </TableCell>
                    <TableCell>
                      <StatusBadge status={row.status} />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function ProxyMetrics() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [range, setRange] = useState<RangeValue>('1h')
  const [filter, setFilter] = useState<ProxyFilter>({
    projectId: null,
    environmentId: null,
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy')

  const isFiltered = filter.projectId != null

  // Full-width like Monitoring.tsx — the app layout wrapper supplies the
  // outer padding, so no container/max-w here.
  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h2 className="text-2xl font-bold tracking-tight">Proxy</h2>
            <p className="text-muted-foreground">
              Hot-path traffic and latency metrics for the control-plane proxy
            </p>
          </div>
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
            <FilterBar filter={filter} onChange={setFilter} />
            <div className="flex items-center gap-1">
              {RANGE_OPTIONS.map((opt) => (
                <Button
                  key={opt.value}
                  variant={range === opt.value ? 'default' : 'outline'}
                  size="sm"
                  onClick={() => setRange(opt.value)}
                >
                  {opt.label}
                </Button>
              ))}
            </div>
          </div>
        </div>

        {isFiltered ? (
          <>
            <FilteredSummaryStats range={range} filter={filter} />
            <FilteredCharts range={range} filter={filter} />
          </>
        ) : (
          <>
            <NodeSummaryStats range={range} />
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              {NODE_PANELS.map((panel) => (
                <NodeMetricPanel
                  key={panel.title}
                  panel={panel}
                  range={range}
                />
              ))}
            </div>
          </>
        )}

        <TrafficByProject range={range} filter={filter} />
      </div>
    </div>
  )
}
