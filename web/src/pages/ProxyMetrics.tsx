/**
 * ProxyMetrics — charts for the proxy hot-path metrics written by the
 * control-plane node (node id 0).
 *
 * Route: /proxy
 *
 * Panels:
 *   - Requests by status class (proxy.requests + 2xx/4xx/5xx, multi-series)
 *   - Error rate % (proxy.error_rate_percent)
 *   - Latency percentiles (p50/p95/p99, multi-series)
 *   - Average request duration (proxy.request_duration_avg_ms)
 *
 * All data comes from the generated SDK binding for
 * `GET /nodes/{id}/metrics` — never hand-rolled fetch.
 */

import { nodeMetricsGetRangeOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQueries } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
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

// ---------------------------------------------------------------------------
// Chart panel — fetches one query per series and merges them on timestamp
// ---------------------------------------------------------------------------

type PanelProps = {
  panel: PanelDef
  range: RangeValue
}

function MetricPanel({ panel, range }: PanelProps) {
  const results = useQueries({
    queries: panel.series.map((s) => ({
      ...nodeMetricsGetRangeOptions({
        path: { id: CONTROL_PLANE_NODE_ID },
        query: { metric: s.metric, range },
      }),
      staleTime: 15_000,
      refetchInterval: 30_000,
      retry: 1,
    })),
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
                  contentStyle={{
                    fontSize: 12,
                    backgroundColor: 'hsl(var(--popover))',
                    border: '1px solid hsl(var(--border))',
                    borderRadius: '6px',
                    color: 'hsl(var(--popover-foreground))',
                    boxShadow: '0 4px 6px -1px rgb(0 0 0 / 0.3)',
                    padding: '6px 10px',
                  }}
                  labelStyle={{
                    color: 'hsl(var(--muted-foreground))',
                    fontSize: 11,
                    marginBottom: 2,
                  }}
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
// Page
// ---------------------------------------------------------------------------

export default function ProxyMetrics() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [range, setRange] = useState<RangeValue>('1h')

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy')

  return (
    <div className="container max-w-7xl mx-auto py-8">
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

        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          {PANELS.map((panel) => (
            <MetricPanel key={panel.title} panel={panel} range={range} />
          ))}
        </div>
      </div>
    </div>
  )
}
