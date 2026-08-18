/**
 * ProxyMetrics — charts for proxy hot-path traffic.
 *
 * Route: /proxy
 *
 * Two data sources, switched by the project/environment filter:
 *   - "All projects" (default): process-wide proxy.* node metrics on the
 *     control-plane node (id 0). These have no project dimension by design.
 *   - Project/environment filtered: proxy-log-derived time buckets from
 *     GET /proxy-logs/stats/time-buckets (request/error counts, avg + p50/p95/p99
 *     latency, bandwidth).
 *
 * All data comes from generated SDK bindings — never hand-rolled fetch.
 */

import {
  getEnvironmentsOptions,
  getProjectOptions,
  getProjectsHealthOptions,
  getProjectsOptions,
  getTimeBucketStatsOptions,
  nodeMetricsGetRangeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { TimeBucketStats } from '@/api/client/types.gen'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { DateRangePicker } from '@/components/ui/date-range-picker'
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
import {
  formatProxyTimeLabel,
  PROXY_MAX_WINDOW_DAYS,
  PROXY_MAX_WINDOW_DAYS_SCOPED,
  PROXY_RANGE_PRESETS,
  proxyWindowTooWide,
  resolveProxyWindow,
  type ProxyRangeValue,
  type ResolvedProxyWindow,
} from '@/lib/proxy-metrics-window'
import { useQueries, useQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import type { DateRange } from 'react-day-picker'
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
      'Upstream duration p50 / p95 / p99 (proxied requests only: connect + processing + TTFB). Includes WebSocket/SSE sessions, whose time-to-first-header is a real backend latency even though their total duration is not',
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
      'Mean request duration per interval, split into backend time and proxy overhead (proxied requests only for the split). Excludes WebSocket/SSE sessions, whose duration is a connection lifetime rather than a latency — see the streaming panel below',
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
  {
    title: 'Proxy overhead percentiles',
    description:
      'Proxy self time p50 / p95 / p99. Read these alongside the mean above: a mean that moves while the percentiles stay flat is a handful of outlier requests, not a broad latency regression',
    series: [
      {
        metric: 'proxy.self_duration_p50_ms',
        dataKey: 'proxy.self_duration_p50_ms',
        label: 'p50',
        color: '#16a34a',
      },
      {
        metric: 'proxy.self_duration_p95_ms',
        dataKey: 'proxy.self_duration_p95_ms',
        label: 'p95',
        color: '#d97706',
      },
      {
        metric: 'proxy.self_duration_p99_ms',
        dataKey: 'proxy.self_duration_p99_ms',
        label: 'p99',
        color: '#dc2626',
      },
    ],
    valueFormatter: formatMs,
  },
  {
    title: 'Streaming sessions',
    description:
      'WebSocket tunnels and SSE streams that closed, averaged per collection interval (not a total for the bucket). These are held open deliberately — up to 1h idle for WebSockets — so activity here is expected and is not proxy latency',
    series: [
      {
        metric: 'proxy.streaming_sessions',
        dataKey: 'proxy.streaming_sessions',
        label: 'Sessions closed',
        color: '#7c3aed',
      },
    ],
    valueFormatter: formatCount,
  },
  {
    title: 'Streaming session lifetime',
    description: 'Mean time a WebSocket/SSE session stayed open before closing',
    series: [
      {
        metric: 'proxy.streaming_duration_avg_ms',
        dataKey: 'proxy.streaming_duration_avg_ms',
        label: 'Mean lifetime',
        color: '#7c3aed',
      },
    ],
    valueFormatter: formatMs,
  },
]

/**
 * File-descriptor panels — the socket-exhaustion signal.
 *
 * Every socket the proxy holds is a file descriptor, so these two series are
 * how close the machine is to refusing new connections. They are separate from
 * NODE_PANELS because they describe the host rather than proxy traffic, and
 * they are what the seeded `node.fd_percent` / `node.process_fd_percent` alert
 * rules watch.
 *
 * Linux-only: both are read from `/proc`, so a macOS dev box shows the empty
 * state rather than a broken chart.
 */
const FD_PANELS: NodePanelDef[] = [
  {
    title: 'File descriptors in use',
    description:
      'How close the host is to running out of file descriptors — which is how it runs out of sockets. System-wide is against /proc/sys/fs/file-nr; process is the temps binary against its own RLIMIT_NOFILE',
    series: [
      {
        metric: 'node.fd_percent',
        dataKey: 'node.fd_percent',
        label: 'System-wide',
        color: '#dc2626',
      },
      {
        metric: 'node.process_fd_percent',
        dataKey: 'node.process_fd_percent',
        label: 'This process',
        color: '#d97706',
      },
    ],
    valueFormatter: formatPercent,
  },
  {
    title: 'Open file descriptors',
    description:
      'Absolute counts, for capacity planning. The process count is always reported; its percentage only exists when RLIMIT_NOFILE is finite',
    series: [
      {
        metric: 'node.fd_allocated',
        dataKey: 'node.fd_allocated',
        label: 'System-wide',
        color: '#2563eb',
      },
      {
        metric: 'node.process_open_fds',
        dataKey: 'node.process_open_fds',
        label: 'This process',
        color: '#16a34a',
      },
    ],
    valueFormatter: formatCount,
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

/** Shared query options for one proxy metric series on the CP node. */
function proxySeriesQuery(metric: string, window: ResolvedProxyWindow) {
  return {
    ...nodeMetricsGetRangeOptions({
      path: { id: CONTROL_PLANE_NODE_ID },
      query: window.rangeParam
        ? { metric, range: window.rangeParam }
        : {
            metric,
            start_time: window.startIso,
            end_time: window.endIso,
          },
    }),
    staleTime: 15_000,
    refetchInterval: 30_000,
    retry: 1,
  }
}

/** Memoized window bounds for the selected range (stable query keys). */
function useResolvedWindow(
  range: ProxyRangeValue,
  custom: DateRange | undefined
) {
  return useMemo(
    () => resolveProxyWindow(range, custom, new Date()),
    [range, custom?.from?.getTime(), custom?.to?.getTime()]
  )
}

/**
 * Proxy-log time buckets for the filtered view. Identical options across the
 * stat cards and every chart panel, so React Query dedupes to one request.
 */
function useBucketStats(window: ResolvedProxyWindow, filter: ProxyFilter) {
  return useQuery({
    ...getTimeBucketStatsOptions({
      query: {
        start_time: window.startIso,
        end_time: window.endIso,
        bucket_interval: window.bucketInterval,
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
  window,
  emptyText = 'No proxy metrics yet — data appears within a minute of traffic',
}: {
  panel: NodePanelDef
  window: ResolvedProxyWindow
  emptyText?: string
}) {
  const results = useQueries({
    queries: panel.series.map((s) => proxySeriesQuery(s.metric, window)),
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
        label: formatProxyTimeLabel(p.time, window.showDate),
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
      emptyText={emptyText}
    />
  )
}

// ---------------------------------------------------------------------------
// Filtered charts (proxy-log time buckets)
// ---------------------------------------------------------------------------

function bucketChartRows(
  stats: TimeBucketStats[],
  window: ResolvedProxyWindow
) {
  return stats.map((b) => ({
    time: b.bucket,
    label: formatProxyTimeLabel(b.bucket, window.showDate),
    request_count: b.request_count,
    error_count: b.error_count,
    error_rate:
      b.request_count > 0 ? (b.error_count / b.request_count) * 100 : 0,
    avg_response_time_ms: b.avg_response_time_ms,
    p50_response_time_ms: b.p50_response_time_ms,
    p95_response_time_ms: b.p95_response_time_ms,
    p99_response_time_ms: b.p99_response_time_ms,
    total_request_bytes: b.total_request_bytes,
    total_response_bytes: b.total_response_bytes,
  }))
}

function FilteredCharts({
  window,
  filter,
}: {
  window: ResolvedProxyWindow
  filter: ProxyFilter
}) {
  const q = useBucketStats(window, filter)
  const stats = q.data?.stats ?? []
  const data = bucketChartRows(stats, window)

  const shared = {
    data,
    isPending: q.isPending,
    errorText: q.isError ? 'Failed to load proxy log statistics' : null,
    emptyText: 'No proxy logs for this selection in the window',
  }

  return (
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
        title="Latency percentiles"
        description="Request duration p50 / p95 / p99, from proxy logs"
        series={[
          {
            dataKey: 'p50_response_time_ms',
            label: 'p50',
            color: '#16a34a',
          },
          {
            dataKey: 'p95_response_time_ms',
            label: 'p95',
            color: '#d97706',
          },
          {
            dataKey: 'p99_response_time_ms',
            label: 'p99',
            color: '#dc2626',
          },
        ]}
        valueFormatter={formatMs}
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
function NodeSummaryStats({ window }: { window: ResolvedProxyWindow }) {
  // Same query keys the chart panels use — React Query dedupes the fetches.
  const [requests, errors5xx, p95, destProject, destConsole, destOther] =
    useQueries({
      queries: [
        proxySeriesQuery('proxy.requests', window),
        proxySeriesQuery('proxy.requests_5xx', window),
        proxySeriesQuery('proxy.request_duration_p95_ms', window),
        // Destination split — same options the "Requests by destination"
        // panel uses, so React Query dedupes the fetches.
        proxySeriesQuery('proxy.requests_project', window),
        proxySeriesQuery('proxy.requests_console', window),
        proxySeriesQuery('proxy.requests_other', window),
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
            ? `${(totalRequests / window.durationSeconds).toFixed(2)}/s`
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
  window,
  filter,
}: {
  window: ResolvedProxyWindow
  filter: ProxyFilter
}) {
  const q = useBucketStats(window, filter)
  const stats = q.data?.stats ?? []

  const totalRequests = stats.reduce((acc, b) => acc + b.request_count, 0)
  const totalErrors = stats.reduce((acc, b) => acc + b.error_count, 0)
  const latestP95 = [...stats]
    .reverse()
    .find((b) => b.request_count > 0)?.p95_response_time_ms

  const ok = !q.isPending && !q.isError

  return (
    <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
      <StatCard
        title="Requests/s"
        isPending={q.isPending}
        value={
          ok ? `${(totalRequests / window.durationSeconds).toFixed(2)}/s` : null
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
        title="p95 latency"
        isPending={q.isPending}
        value={ok && latestP95 != null ? formatMs(latestP95) : null}
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

const TRAFFIC_PAGE_SIZE = 20

function TrafficByProject({
  window,
  filter,
}: {
  window: ResolvedProxyWindow
  filter: ProxyFilter
}) {
  const [sortKey, setSortKey] = useState<TrafficSortKey>('total_requests')
  const [sortDesc, setSortDesc] = useState(true)
  const [page, setPage] = useState(1)
  const { startIso, endIso } = window

  // Defer fetching until the card is actually scrolled into view — on the
  // unfiltered view it sits below the node metrics panels, so a page load
  // that never scrolls this far shouldn't pay for it.
  const containerRef = useRef<HTMLDivElement>(null)
  const [visible, setVisible] = useState(false)
  useEffect(() => {
    const node = containerRef.current
    if (!node || visible) return
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setVisible(true)
          observer.disconnect()
        }
      },
      { rootMargin: '200px 0px', threshold: 0 }
    )
    observer.observe(node)
    return () => observer.disconnect()
  }, [visible])

  // Reset to page 1 when the project filter changes, without an extra
  // render-then-effect round trip (react.dev/learn/you-might-not-need-an-effect).
  const [prevFilterProjectId, setPrevFilterProjectId] = useState(
    filter.projectId
  )
  if (filter.projectId !== prevFilterProjectId) {
    setPrevFilterProjectId(filter.projectId)
    setPage(1)
  }

  const isFilteredToOneProject = filter.projectId != null

  // Filtered to one project: fetch that project directly instead of paging
  // through the full list looking for it. Unfiltered: page through projects
  // TRAFFIC_PAGE_SIZE at a time instead of pulling up to 100 at once.
  const singleProjectQ = useQuery({
    ...getProjectOptions({ path: { id: filter.projectId ?? 0 } }),
    enabled: visible && isFilteredToOneProject,
  })
  const projectsPageQ = useQuery({
    ...getProjectsOptions({ query: { page, per_page: TRAFFIC_PAGE_SIZE } }),
    enabled: visible && !isFilteredToOneProject,
    staleTime: 60_000,
  })

  const projects = isFilteredToOneProject
    ? singleProjectQ.data
      ? [singleProjectQ.data]
      : []
    : (projectsPageQ.data?.projects ?? [])
  const total = projectsPageQ.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / TRAFFIC_PAGE_SIZE))

  const idsParam = projects.map((p) => p.id).join(',')
  const healthQ = useQuery({
    ...getProjectsHealthOptions({
      query: {
        project_ids: idsParam,
        start_time: startIso,
        end_time: endIso,
      },
    }),
    enabled: visible && projects.length > 0,
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

  const isLoadingProjects = isFilteredToOneProject
    ? singleProjectQ.isPending
    : projectsPageQ.isPending
  const hasError =
    (isFilteredToOneProject ? singleProjectQ.isError : projectsPageQ.isError) ||
    healthQ.isError
  const isPending =
    !visible || isLoadingProjects || (projects.length > 0 && healthQ.isPending)

  return (
    <Card ref={containerRef}>
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
        ) : hasError ? (
          <div className="py-8 text-center text-sm text-rose-500">
            Failed to load project traffic
          </div>
        ) : sorted.length === 0 ? (
          <div className="py-8 text-center text-sm text-muted-foreground">
            No projects yet
          </div>
        ) : (
          <>
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
            {!isFilteredToOneProject && total > TRAFFIC_PAGE_SIZE && (
              <div className="mt-4 flex items-center justify-center gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                  disabled={page === 1}
                >
                  Previous
                </Button>
                <span className="text-sm text-muted-foreground">
                  Page {page} of {totalPages}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                  disabled={page >= totalPages}
                >
                  Next
                </Button>
              </div>
            )}
          </>
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
  const [range, setRange] = useState<ProxyRangeValue>('1h')
  const [customRange, setCustomRange] = useState<DateRange | undefined>()
  const [filter, setFilter] = useState<ProxyFilter>({
    projectId: null,
    environmentId: null,
  })
  const resolved = useResolvedWindow(range, customRange)
  const isFiltered = filter.projectId != null
  const tooWide = proxyWindowTooWide(resolved, isFiltered)
  const maxDays = isFiltered
    ? PROXY_MAX_WINDOW_DAYS_SCOPED
    : PROXY_MAX_WINDOW_DAYS

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy')

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
              {PROXY_RANGE_PRESETS.map((opt) => (
                <Button
                  key={opt.value}
                  variant={range === opt.value ? 'default' : 'outline'}
                  size="sm"
                  onClick={() => setRange(opt.value)}
                >
                  {opt.label}
                </Button>
              ))}
              <Button
                variant={range === 'custom' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setRange('custom')}
              >
                Custom
              </Button>
            </div>
            {range === 'custom' && (
              <DateRangePicker
                date={customRange}
                onDateChange={setCustomRange}
                showTime
                className="w-full sm:w-[300px]"
              />
            )}
          </div>
        </div>

        {tooWide ? (
          <p className="rounded-md border border-dashed px-4 py-8 text-center text-sm text-muted-foreground">
            Custom range exceeds the {maxDays}-day maximum
            {isFiltered ? ' for a project' : ''}. Narrow the window, or request
            older data {maxDays} days at a time.
          </p>
        ) : isFiltered ? (
          <>
            <FilteredSummaryStats window={resolved} filter={filter} />
            <FilteredCharts window={resolved} filter={filter} />
          </>
        ) : (
          <>
            <NodeSummaryStats window={resolved} />
            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              {NODE_PANELS.map((panel) => (
                <NodeMetricPanel
                  key={panel.title}
                  panel={panel}
                  window={resolved}
                />
              ))}
            </div>
            <div>
              <h3 className="text-lg font-semibold tracking-tight">
                Sockets &amp; file descriptors
              </h3>
              <p className="mb-4 text-sm text-muted-foreground">
                Sockets are file descriptors, so descriptor exhaustion is how
                the proxy stops accepting connections. Collected on Linux only.
                Temps alerts on these automatically —{' '}
                <Link
                  to="/monitoring/rules"
                  className="underline underline-offset-2 hover:text-foreground"
                >
                  tune the thresholds in Monitoring settings
                </Link>
                .
              </p>
              <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
                {FD_PANELS.map((panel) => (
                  <NodeMetricPanel
                    key={panel.title}
                    panel={panel}
                    window={resolved}
                    emptyText="No file-descriptor samples in this window. These are read from /proc, so they are only collected when temps runs on Linux."
                  />
                ))}
              </div>
            </div>
          </>
        )}

        {!tooWide && <TrafficByProject window={resolved} filter={filter} />}
      </div>
    </div>
  )
}
