import { useMemo, useState } from 'react'
import { useQueries, useQuery } from '@tanstack/react-query'
import {
  containerMetricsGetHistoryOptions,
  listContainerHistoryOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  ChartLegend,
  ChartLegendContent,
} from '@/components/ui/chart'
import { Line, LineChart, XAxis, YAxis, CartesianGrid } from 'recharts'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Button } from '@/components/ui/button'
import { format } from 'date-fns'
import { Link } from 'react-router'
import { LineChart as LineChartIcon } from 'lucide-react'

// Theme only defines 5 chart colors (web/src/globals.css); beyond 5
// containers colors repeat but the legend label still disambiguates.
const CHART_COLORS = [
  'var(--chart-1)',
  'var(--chart-2)',
  'var(--chart-3)',
  'var(--chart-4)',
  'var(--chart-5)',
]

const RANGES = [
  { value: '1h', label: 'Last hour' },
  { value: '6h', label: 'Last 6 hours' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
] as const
type RangeValue = (typeof RANGES)[number]['value']

interface ContainerSeries {
  key: string
  label: string
  color: string
}

interface MergedPoint {
  time: string
  timestamp: number
  [seriesKey: string]: number | string
}

/** Merge independently-fetched per-container history series onto one time
 *  axis, keyed by the bucket timestamp the backend already returns —
 *  TimescaleDB/ClickHouse bucket on absolute clock time, so buckets from
 *  concurrent requests line up without needing to resample here. */
function mergeHistory(
  series: { key: string; points: { time: string; value: number }[] }[]
): MergedPoint[] {
  const rows = new Map<string, MergedPoint>()
  for (const { key, points } of series) {
    for (const p of points) {
      let row = rows.get(p.time)
      if (!row) {
        row = { time: p.time, timestamp: new Date(p.time).getTime() }
        rows.set(p.time, row)
      }
      row[key] = p.value
    }
  }
  return Array.from(rows.values()).sort((a, b) => a.timestamp - b.timestamp)
}

function formatTimeTick(iso: string | number, range: RangeValue): string {
  const d = new Date(iso)
  return range === '24h' || range === '7d'
    ? format(d, 'MMM d HH:mm')
    : format(d, 'HH:mm')
}

function formatNetworkRate(kbs: number): string {
  if (kbs >= 1024) return `${(kbs / 1024).toFixed(1)} MB/s`
  if (kbs >= 1) return `${kbs.toFixed(1)} KB/s`
  return `${(kbs * 1024).toFixed(0)} B/s`
}

function metricTooltipRow(
  label: React.ReactNode,
  value: string,
  color?: string
) {
  return (
    <div className="flex w-full items-center justify-between gap-4">
      <div className="flex items-center gap-1.5">
        <span
          className="h-2.5 w-2.5 shrink-0 rounded-[2px]"
          style={{ backgroundColor: color }}
        />
        <span className="text-muted-foreground">{label}</span>
      </div>
      <span className="font-mono font-medium tabular-nums text-foreground">
        {value}
      </span>
    </div>
  )
}

/** `@hey-api/client-fetch` throws the parsed RFC 7807 Problem body
 *  ({ detail, title, status }) on a failed request — same convention
 *  MonitoringCard.tsx uses for its "not enabled" detection. */
function metricsErrorDetail(err: unknown): string {
  if (err == null) return ''
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  const problem = err as { detail?: string; title?: string }
  return problem.detail ?? problem.title ?? ''
}

interface EnvironmentMetricsChartsProps {
  projectId: number
  environmentId: number
}

export function EnvironmentMetricsCharts({
  projectId,
  environmentId,
}: EnvironmentMetricsChartsProps) {
  const [range, setRange] = useState<RangeValue>('1h')
  // Long-lived environments accumulate one history entry per past redeploy;
  // charting all of them by default turns the legend into unreadable noise
  // (one line + label per replaced container). Default to the current
  // container(s) only, with an explicit toggle to bring the rest back.
  const [showReplaced, setShowReplaced] = useState(false)

  const historyQuery = useQuery({
    ...listContainerHistoryOptions({
      path: { project_id: projectId, environment_id: environmentId },
    }),
    staleTime: 30_000,
  })

  const entries = useMemo(
    () =>
      [...(historyQuery.data?.containers ?? [])].sort((a, b) =>
        a.container_id.localeCompare(b.container_id)
      ),
    [historyQuery.data]
  )

  const currentCount = useMemo(
    () => entries.filter((c) => c.is_current).length,
    [entries]
  )
  const replacedCount = entries.length - currentCount

  // If nothing is currently running there's no "current" set to fall back
  // to, so show full history rather than an empty chart.
  const visibleEntries = useMemo(
    () =>
      showReplaced || currentCount === 0
        ? entries
        : entries.filter((c) => c.is_current),
    [entries, showReplaced, currentCount]
  )

  const containerSeries = useMemo(() => {
    const map = new Map<string, ContainerSeries>()
    visibleEntries.forEach((c, i) => {
      map.set(c.container_id, {
        key: `c${i}`,
        label:
          (c.container_name || c.container_id).replace(/^\//, '') +
          (c.is_current ? '' : ' (replaced)'),
        color: CHART_COLORS[i % CHART_COLORS.length],
      })
    })
    return map
  }, [visibleEntries])

  const historyOptionsFor = (containerId: string, metric: string) => ({
    ...containerMetricsGetHistoryOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
        container_id: containerId,
      },
      query: { metric, range },
    }),
    staleTime: 30_000,
    refetchInterval: 30_000,
    // Metrics store disabled -> the endpoint 503s for every container;
    // avoid retrying a request that can't succeed.
    retry: false,
  })

  const cpuQueries = useQueries({
    queries: visibleEntries.map((c) =>
      historyOptionsFor(c.container_id, 'container.cpu_percent')
    ),
  })
  const memQueries = useQueries({
    queries: visibleEntries.map((c) =>
      historyOptionsFor(c.container_id, 'container.memory_used_bytes')
    ),
  })
  const netRxQueries = useQueries({
    queries: visibleEntries.map((c) =>
      historyOptionsFor(c.container_id, 'container.network_rx_bytes_delta')
    ),
  })
  const netTxQueries = useQueries({
    queries: visibleEntries.map((c) =>
      historyOptionsFor(c.container_id, 'container.network_tx_bytes_delta')
    ),
  })

  const isLoading = historyQuery.isLoading
  const notConfigured =
    visibleEntries.length > 0 &&
    cpuQueries.length === visibleEntries.length &&
    cpuQueries.every(
      (q) =>
        q.isError &&
        metricsErrorDetail(q.error).toLowerCase().includes('not enabled')
    )

  const cpuChartConfig = useMemo(() => {
    const config: ChartConfig = {}
    for (const series of containerSeries.values()) {
      config[`cpu_${series.key}`] = { label: series.label, color: series.color }
    }
    return config
  }, [containerSeries])

  const memChartConfig = useMemo(() => {
    const config: ChartConfig = {}
    for (const series of containerSeries.values()) {
      config[`mem_${series.key}`] = { label: series.label, color: series.color }
    }
    return config
  }, [containerSeries])

  const netChartConfig = useMemo(() => {
    const config: ChartConfig = {}
    for (const series of containerSeries.values()) {
      config[`rx_${series.key}`] = {
        label: `${series.label} — in`,
        color: series.color,
      }
      config[`tx_${series.key}`] = {
        label: `${series.label} — out`,
        color: series.color,
      }
    }
    return config
  }, [containerSeries])

  const cpuData = useMemo(
    () =>
      mergeHistory(
        visibleEntries.map((c, i) => ({
          key: `cpu_${containerSeries.get(c.container_id)!.key}`,
          points: cpuQueries[i]?.data ?? [],
        }))
      ),
    [visibleEntries, cpuQueries, containerSeries]
  )

  const memData = useMemo(
    () =>
      mergeHistory(
        visibleEntries.map((c, i) => ({
          key: `mem_${containerSeries.get(c.container_id)!.key}`,
          points: (memQueries[i]?.data ?? []).map((p) => ({
            time: p.time,
            value: p.value / (1024 * 1024),
          })),
        }))
      ),
    [visibleEntries, memQueries, containerSeries]
  )

  const netData = useMemo(
    () =>
      mergeHistory([
        ...visibleEntries.map((c, i) => ({
          key: `rx_${containerSeries.get(c.container_id)!.key}`,
          points: (netRxQueries[i]?.data ?? []).map((p) => ({
            time: p.time,
            value: p.value / 1024,
          })),
        })),
        ...visibleEntries.map((c, i) => ({
          key: `tx_${containerSeries.get(c.container_id)!.key}`,
          points: (netTxQueries[i]?.data ?? []).map((p) => ({
            time: p.time,
            value: p.value / 1024,
          })),
        })),
      ]),
    [visibleEntries, netRxQueries, netTxQueries, containerSeries]
  )

  if (isLoading) {
    return (
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <Skeleton className="h-[240px] w-full" />
        <Skeleton className="h-[240px] w-full" />
      </div>
    )
  }

  if (entries.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
        No containers have ever run in this environment
      </div>
    )
  }

  if (notConfigured) {
    return (
      <div className="flex flex-col items-center justify-center gap-3 rounded-lg border border-dashed py-12 text-center">
        <LineChartIcon
          className="size-8 text-muted-foreground"
          aria-hidden="true"
        />
        <div className="space-y-1">
          <p className="text-sm font-medium">
            Metrics history isn&apos;t collected yet
          </p>
          <p className="max-w-md text-sm text-muted-foreground">
            Once enabled, this view shows CPU, memory, and network usage over
            time for every container this environment has run — including ones
            replaced by a later redeploy — filterable by 1h/6h/24h/7d.
          </p>
        </div>
        <Link
          to="/settings/metrics-monitoring"
          className="text-sm font-medium text-primary hover:underline"
        >
          Enable metrics collection →
        </Link>
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-xs text-muted-foreground">
          {currentCount} running
          {replacedCount > 0 && (
            <>
              {' · '}
              <Button
                type="button"
                variant="link"
                size="sm"
                className="h-auto p-0 text-xs text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground"
                onClick={() => setShowReplaced((v) => !v)}
              >
                {replacedCount} replaced by earlier redeploys —{' '}
                {showReplaced ? 'hide' : 'show'}
              </Button>
            </>
          )}
        </p>
        <Select value={range} onValueChange={(v) => setRange(v as RangeValue)}>
          <SelectTrigger className="w-[160px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {RANGES.map((r) => (
              <SelectItem key={r.value} value={r.value}>
                {r.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        {/* CPU Line Chart */}
        <div>
          <p className="text-sm font-medium mb-2">CPU Usage</p>
          {cpuData.length > 0 ? (
            <ChartContainer
              config={cpuChartConfig}
              className="h-[240px] w-full"
            >
              <LineChart
                data={cpuData}
                margin={{ left: 12, right: 12, top: 8, bottom: 0 }}
              >
                <CartesianGrid
                  strokeDasharray="3 3"
                  vertical={false}
                  className="stroke-muted/30"
                />
                <XAxis
                  dataKey="time"
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  minTickGap={50}
                  tick={{ fontSize: 11 }}
                  tickFormatter={(v) => formatTimeTick(v, range)}
                />
                <YAxis
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  tick={{ fontSize: 11 }}
                  domain={[0, (max: number) => Math.max(max * 1.2, 10)]}
                  tickFormatter={(v) => `${v}%`}
                  width={45}
                />
                <ChartTooltip
                  labelFormatter={(v) =>
                    typeof v === 'string' || typeof v === 'number'
                      ? formatTimeTick(v, range)
                      : ''
                  }
                  content={
                    <ChartTooltipContent
                      formatter={(value, name, item) =>
                        metricTooltipRow(
                          cpuChartConfig[name as string]?.label ?? name,
                          `${Number(value).toFixed(2)}%`,
                          item?.color
                        )
                      }
                    />
                  }
                />
                <ChartLegend content={<ChartLegendContent />} />
                {Array.from(containerSeries.values()).map((series) => (
                  <Line
                    key={series.key}
                    dataKey={`cpu_${series.key}`}
                    name={`cpu_${series.key}`}
                    type="monotone"
                    stroke={`var(--color-cpu_${series.key})`}
                    strokeWidth={2}
                    dot={false}
                    activeDot={{ r: 4 }}
                    connectNulls
                  />
                ))}
              </LineChart>
            </ChartContainer>
          ) : (
            <div className="flex h-[240px] items-center justify-center text-sm text-muted-foreground">
              No data in this range
            </div>
          )}
        </div>

        {/* Memory Line Chart */}
        <div>
          <p className="text-sm font-medium mb-2">Memory Usage</p>
          {memData.length > 0 ? (
            <ChartContainer
              config={memChartConfig}
              className="h-[240px] w-full"
            >
              <LineChart
                data={memData}
                margin={{ left: 12, right: 12, top: 8, bottom: 0 }}
              >
                <CartesianGrid
                  strokeDasharray="3 3"
                  vertical={false}
                  className="stroke-muted/30"
                />
                <XAxis
                  dataKey="time"
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  minTickGap={50}
                  tick={{ fontSize: 11 }}
                  tickFormatter={(v) => formatTimeTick(v, range)}
                />
                <YAxis
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  tick={{ fontSize: 11 }}
                  domain={[0, 'auto']}
                  tickFormatter={(v) => `${v} MB`}
                  width={55}
                />
                <ChartTooltip
                  labelFormatter={(v) =>
                    typeof v === 'string' || typeof v === 'number'
                      ? formatTimeTick(v, range)
                      : ''
                  }
                  content={
                    <ChartTooltipContent
                      formatter={(value, name, item) =>
                        metricTooltipRow(
                          memChartConfig[name as string]?.label ?? name,
                          `${Number(value).toFixed(1)} MB`,
                          item?.color
                        )
                      }
                    />
                  }
                />
                <ChartLegend content={<ChartLegendContent />} />
                {Array.from(containerSeries.values()).map((series) => (
                  <Line
                    key={series.key}
                    dataKey={`mem_${series.key}`}
                    name={`mem_${series.key}`}
                    type="monotone"
                    stroke={`var(--color-mem_${series.key})`}
                    strokeWidth={2}
                    dot={false}
                    activeDot={{ r: 4 }}
                    connectNulls
                  />
                ))}
              </LineChart>
            </ChartContainer>
          ) : (
            <div className="flex h-[240px] items-center justify-center text-sm text-muted-foreground">
              No data in this range
            </div>
          )}
        </div>

        {/* Network I/O Line Chart */}
        <div className="lg:col-span-2">
          <p className="text-sm font-medium mb-2">Network I/O</p>
          {netData.length > 0 ? (
            <ChartContainer
              config={netChartConfig}
              className="h-[240px] w-full"
            >
              <LineChart
                data={netData}
                margin={{ left: 12, right: 12, top: 8, bottom: 0 }}
              >
                <CartesianGrid
                  strokeDasharray="3 3"
                  vertical={false}
                  className="stroke-muted/30"
                />
                <XAxis
                  dataKey="time"
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  minTickGap={50}
                  tick={{ fontSize: 11 }}
                  tickFormatter={(v) => formatTimeTick(v, range)}
                />
                <YAxis
                  tickLine={false}
                  axisLine={false}
                  tickMargin={8}
                  tick={{ fontSize: 11 }}
                  domain={[0, 'auto']}
                  tickFormatter={(v) => formatNetworkRate(v)}
                  width={70}
                />
                <ChartTooltip
                  labelFormatter={(v) =>
                    typeof v === 'string' || typeof v === 'number'
                      ? formatTimeTick(v, range)
                      : ''
                  }
                  content={
                    <ChartTooltipContent
                      formatter={(value, name, item) =>
                        metricTooltipRow(
                          netChartConfig[name as string]?.label ?? name,
                          formatNetworkRate(Number(value)),
                          item?.color
                        )
                      }
                    />
                  }
                />
                <ChartLegend content={<ChartLegendContent />} />
                {Array.from(containerSeries.values()).flatMap((series) => [
                  <Line
                    key={`rx-${series.key}`}
                    dataKey={`rx_${series.key}`}
                    name={`rx_${series.key}`}
                    type="monotone"
                    stroke={`var(--color-rx_${series.key})`}
                    strokeWidth={2}
                    dot={false}
                    activeDot={{ r: 4 }}
                    connectNulls
                  />,
                  <Line
                    key={`tx-${series.key}`}
                    dataKey={`tx_${series.key}`}
                    name={`tx_${series.key}`}
                    type="monotone"
                    stroke={`var(--color-tx_${series.key})`}
                    strokeWidth={2}
                    strokeDasharray="4 3"
                    dot={false}
                    activeDot={{ r: 4 }}
                    connectNulls
                  />,
                ])}
              </LineChart>
            </ChartContainer>
          ) : (
            <div className="flex h-[240px] items-center justify-center text-sm text-muted-foreground">
              No data in this range
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
