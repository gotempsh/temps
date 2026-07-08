/**
 * ProxyMetrics — charts for the proxy hot-path metrics written by the
 * control-plane node (node id 0).
 *
 * Route: /proxy
 *
 * Sections:
 *   - Summary stat cards (req/s, total requests, error rate, p95) computed
 *     client-side from the same series the charts fetch
 *   - Chart panels: requests by status class, error rate %, latency
 *     percentiles, average duration
 *   - Traffic by project: analytics page-view breakdown per project
 *
 * All data comes from the generated SDK bindings (`GET /nodes/{id}/metrics`
 * and `GET /analytics/general-stats`) — never hand-rolled fetch.
 */

import {
  getGeneralStatsOptions,
  nodeMetricsGetRangeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { TOOLTIP_CONTENT_STYLE, TOOLTIP_LABEL_STYLE } from '@/lib/chart-tooltip'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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

/** One line in a chart panel: metric name + display label + stroke color. */
type SeriesDef = {
  metric: string
  label: string
  color: string
}

type PanelDef = {
  title: string
  description: string
  series: SeriesDef[]
  valueFormatter: (v: number) => string
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

const PANELS: PanelDef[] = [
  {
    title: 'Requests by status class',
    description: 'Per-interval request count, split by response status class',
    series: [
      { metric: 'proxy.requests', label: 'Total', color: '#2563eb' },
      { metric: 'proxy.requests_2xx', label: '2xx', color: '#16a34a' },
      { metric: 'proxy.requests_4xx', label: '4xx', color: '#d97706' },
      { metric: 'proxy.requests_5xx', label: '5xx', color: '#dc2626' },
    ],
    valueFormatter: formatCount,
  },
  {
    title: 'Error rate',
    description: 'Percentage of requests answered with a 5xx status',
    series: [
      {
        metric: 'proxy.error_rate_percent',
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
        label: 'p50',
        color: '#16a34a',
      },
      {
        metric: 'proxy.request_duration_p95_ms',
        label: 'p95',
        color: '#d97706',
      },
      {
        metric: 'proxy.request_duration_p99_ms',
        label: 'p99',
        color: '#dc2626',
      },
    ],
    valueFormatter: formatMs,
  },
  {
    title: 'Average duration',
    description: 'Mean request duration per interval',
    series: [
      {
        metric: 'proxy.request_duration_avg_ms',
        label: 'avg',
        color: '#2563eb',
      },
    ],
    valueFormatter: formatMs,
  },
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/** Format a Date as the analytics API's "YYYY-MM-DD HH:MM:SS". */
function toAnalyticsDate(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
}

// ---------------------------------------------------------------------------
// Summary stat cards
// ---------------------------------------------------------------------------

type StatCardProps = {
  title: string
  value: string | null
  isPending: boolean
}

function StatCard({ title, value, isPending }: StatCardProps) {
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
          <div className="text-2xl font-semibold tracking-tight tabular-nums">
            {value ?? '—'}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function SummaryStats({ range }: { range: RangeValue }) {
  // Same query keys the chart panels use — React Query dedupes the fetches.
  const [requests, errors5xx, p95] = useQueries({
    queries: [
      proxySeriesQuery('proxy.requests', range),
      proxySeriesQuery('proxy.requests_5xx', range),
      proxySeriesQuery('proxy.request_duration_p95_ms', range),
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
  const reqPerSec = totalRequests / RANGE_SECONDS[range]

  return (
    <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
      <StatCard
        title="Requests/s"
        isPending={requests.isPending}
        value={hasRequests ? `${reqPerSec.toFixed(2)}/s` : null}
      />
      <StatCard
        title="Total requests"
        isPending={requests.isPending}
        value={hasRequests ? formatCount(totalRequests) : null}
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

// ---------------------------------------------------------------------------
// Chart panel — fetches one query per series and merges them on timestamp
// ---------------------------------------------------------------------------

type PanelProps = {
  panel: PanelDef
  range: RangeValue
}

function MetricPanel({ panel, range }: PanelProps) {
  const results = useQueries({
    queries: panel.series.map((s) => proxySeriesQuery(s.metric, range)),
  })

  const isPending = results.some((r) => r.isPending)
  const errors = results.filter((r) => r.isError).map((r) => r.error)
  const unavailable =
    errors.length === results.length &&
    errors.length > 0 &&
    errors.every(isMetricsUnavailable)
  const allFailed = errors.length === results.length && errors.length > 0

  // Merge the per-series point arrays into one row per timestamp so recharts
  // can render them as aligned lines. Recomputed per render — a few hundred
  // points at most, so memoization isn't worth an unstable dependency list.
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
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-base">{panel.title}</CardTitle>
        <CardDescription>{panel.description}</CardDescription>
      </CardHeader>
      <CardContent>
        {isPending ? (
          <Skeleton className="h-[220px] w-full" />
        ) : unavailable ? (
          <div className="flex h-[220px] items-center justify-center px-6 text-center text-sm text-muted-foreground">
            Metric collection is not enabled on this server.
          </div>
        ) : allFailed ? (
          <div className="flex h-[220px] items-center justify-center px-6 text-center text-sm text-rose-500">
            Failed to load proxy metrics
          </div>
        ) : chartData.length === 0 ? (
          <div className="flex h-[220px] items-center justify-center px-6 text-center text-sm text-muted-foreground">
            No proxy metrics yet — data appears within a minute of traffic
          </div>
        ) : (
          <div className="h-[220px]">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart
                data={chartData}
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
                  width={52}
                  tickFormatter={panel.valueFormatter}
                />
                <Tooltip
                  wrapperStyle={{ zIndex: 50 }}
                  allowEscapeViewBox={{ x: true, y: true }}
                  contentStyle={TOOLTIP_CONTENT_STYLE}
                  labelStyle={TOOLTIP_LABEL_STYLE}
                  cursor={{ stroke: 'rgba(128,128,128,0.3)', strokeWidth: 1 }}
                  formatter={(v: number, name: string) => [
                    panel.valueFormatter(v),
                    name,
                  ]}
                />
                {panel.series.map((s) => (
                  <Line
                    key={s.metric}
                    type="monotone"
                    dataKey={s.metric}
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
        {panel.series.length > 1 && (
          <div className="mt-2 flex flex-wrap items-center gap-3">
            {panel.series.map((s) => (
              <span
                key={s.metric}
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
// Traffic by project (analytics page-view breakdown)
// ---------------------------------------------------------------------------

type BreakdownSortKey =
  | 'project_name'
  | 'total_page_views'
  | 'total_visits'
  | 'unique_visitors'
  | 'bounce_rate'

const BREAKDOWN_COLUMNS: {
  key: BreakdownSortKey
  label: string
  numeric: boolean
  secondary: boolean
}[] = [
  { key: 'project_name', label: 'Project', numeric: false, secondary: false },
  {
    key: 'total_page_views',
    label: 'Page views',
    numeric: true,
    secondary: false,
  },
  { key: 'total_visits', label: 'Visits', numeric: true, secondary: true },
  {
    key: 'unique_visitors',
    label: 'Unique visitors',
    numeric: true,
    secondary: true,
  },
  { key: 'bounce_rate', label: 'Bounce rate', numeric: true, secondary: true },
]

function TrafficByProject({ range }: { range: RangeValue }) {
  const [sortKey, setSortKey] = useState<BreakdownSortKey>('total_page_views')
  const [sortDesc, setSortDesc] = useState(true)

  // Stable per range selection — an inline `new Date()` in the query options
  // would change the query key every render and refetch forever.
  const { startDate, endDate } = useMemo(() => {
    const end = new Date()
    const start = new Date(end.getTime() - RANGE_SECONDS[range] * 1000)
    return { startDate: toAnalyticsDate(start), endDate: toAnalyticsDate(end) }
  }, [range])

  const q = useQuery({
    ...getGeneralStatsOptions({
      query: {
        start_date: startDate,
        end_date: endDate,
        include_project_breakdown: true,
      },
    }),
    staleTime: 30_000,
  })

  const breakdown = q.data?.project_breakdown ?? []
  const sorted = [...breakdown].sort((a, b) => {
    const av = a[sortKey]
    const bv = b[sortKey]
    let cmp: number
    if (typeof av === 'number' && typeof bv === 'number') {
      cmp = av - bv
    } else {
      cmp = String(av ?? '').localeCompare(String(bv ?? ''))
    }
    return sortDesc ? -cmp : cmp
  })

  const onSort = (key: BreakdownSortKey) => {
    if (key === sortKey) {
      setSortDesc((d) => !d)
    } else {
      setSortKey(key)
      setSortDesc(true)
    }
  }

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-base">Traffic by project</CardTitle>
        <CardDescription>
          Analytics page-view traffic per project over the selected window — not
          raw proxy request counts
        </CardDescription>
      </CardHeader>
      <CardContent>
        {q.isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
          </div>
        ) : q.isError ? (
          <div className="py-8 text-center text-sm text-rose-500">
            Failed to load project traffic
          </div>
        ) : sorted.length === 0 ? (
          <div className="py-8 text-center text-sm text-muted-foreground">
            No analytics traffic recorded in this window
          </div>
        ) : (
          <div className="overflow-x-auto">
            <Table className="min-w-[420px]">
              <TableHeader>
                <TableRow>
                  {BREAKDOWN_COLUMNS.map((col) => (
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
                  <TableRow key={row.project_id}>
                    <TableCell className="font-medium">
                      {row.project_name?.trim() || `Project #${row.project_id}`}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {formatCount(row.total_page_views)}
                    </TableCell>
                    <TableCell className="hidden text-right tabular-nums md:table-cell">
                      {formatCount(row.total_visits)}
                    </TableCell>
                    <TableCell className="hidden text-right tabular-nums md:table-cell">
                      {formatCount(row.unique_visitors)}
                    </TableCell>
                    <TableCell className="hidden text-right tabular-nums md:table-cell">
                      {formatPercent(row.bounce_rate)}
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

        <SummaryStats range={range} />

        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          {PANELS.map((panel) => (
            <MetricPanel key={panel.title} panel={panel} range={range} />
          ))}
        </div>

        <TrafficByProject range={range} />
      </div>
    </div>
  )
}
