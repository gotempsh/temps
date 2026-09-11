// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * ServerMonitoring — resource usage of the machine running the control plane.
 *
 * Route: /monitoring/server (the "Server" section of Monitoring & Alerts).
 *
 * Six panels, in the shape of the Proxy page: CPU, memory and disk usage
 * (current value + progress bar + history), Docker disk usage (docker system
 * df by images / containers / volumes / build cache), block I/O and network
 * I/O. Everything comes from `GET /nodes/0/metrics*` (the node sampler in
 * the proxy process) and `GET /nodes/0/docker-disk-usage`, through the
 * generated SDK bindings.
 */

import {
  nodeDockerDiskUsageGetOptions,
  nodeMetricsGetLatestOptions,
  nodeMetricsGetRangeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { MetricDataPoint } from '@/api/client/types.gen'
import {
  ThresholdLineChart,
  type ThresholdBand,
  type ThresholdLineSeries,
} from '@/components/charts/threshold-line-chart'
import { seriesLineColor } from '@/components/charts/chart-colors'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { useSettings } from '@/hooks/useSettings'
import {
  CONTROL_PLANE_NODE_ID,
  CPU_THRESHOLDS,
  DISK_THRESHOLDS,
  DOCKER_USAGE_SLICES,
  MEMORY_THRESHOLDS,
  STEP_SECONDS,
  formatAge,
  formatBytesBinary,
  formatBytesDecimal,
  formatBytesPerSecond,
  formatDays,
  formatPercent,
  formatRateTick,
  isMetricsUnavailable,
  mergeSeries,
  peakOf,
  projectDisk,
  toRatePerSecond,
  usagePercent,
  usageTone,
  type UsageThresholds,
  type UsageTone,
} from '@/lib/server-monitoring'
import {
  PROXY_RANGE_PRESETS,
  formatProxyTimeLabel,
  type ProxyRangePreset,
} from '@/lib/proxy-metrics-window'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, Pause, Play, RefreshCw, Settings } from 'lucide-react'
import { useState } from 'react'
import { Link, useSearchParams } from 'react-router'

const METRICS_SETTINGS_PATH = '/settings/metrics-monitoring'
const REFRESH_MS = 30_000
/** All panels share one tooltip cursor: hover or arrow through one and the others follow. */
const CHART_SYNC_ID = 'server-monitoring'

const isRangePreset = (v: string | null): v is ProxyRangePreset =>
  PROXY_RANGE_PRESETS.some((p) => p.value === v)

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

function useNodeSeries(
  metric: string,
  range: ProxyRangePreset,
  refetchInterval: number | false
) {
  return useQuery({
    ...nodeMetricsGetRangeOptions({
      path: { id: CONTROL_PLANE_NODE_ID },
      query: { metric, range },
    }),
    staleTime: 15_000,
    refetchInterval,
    retry: false,
  })
}

const lastValue = (points: MetricDataPoint[] | undefined) =>
  points && points.length > 0 ? points[points.length - 1].value : null

// ---------------------------------------------------------------------------
// Presentational pieces
// ---------------------------------------------------------------------------

const TONE_TEXT: Record<UsageTone, string> = {
  good: 'text-emerald-600 dark:text-emerald-400',
  warn: 'text-amber-600 dark:text-amber-400',
  poor: 'text-rose-600 dark:text-rose-400',
}
const TONE_BAR: Record<UsageTone, string> = {
  good: '[&>div]:bg-primary',
  warn: '[&>div]:bg-amber-500',
  poor: '[&>div]:bg-rose-500',
}

type UsageCardProps = {
  title: string
  description: string
  /** Current share of the resource, 0–100. */
  percent: number | null
  /** "1.2 GiB of 8 GiB" — printed beside the percentage. */
  absolute: string | null
  thresholds: UsageThresholds
  /** Muted one-liner under the bar (peak, projection). */
  sub?: string | null
  isPending: boolean
}

function UsageCard({
  title,
  description,
  percent,
  absolute,
  thresholds,
  sub,
  isPending,
}: UsageCardProps) {
  const tone = usageTone(percent, thresholds)
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {title}
        </CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>
        {isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-8 w-24" />
            <Skeleton className="h-2 w-full" />
          </div>
        ) : (
          <>
            <div className="flex items-baseline justify-between gap-3">
              <span
                className={cn(
                  'text-2xl font-semibold tracking-tight tabular-nums',
                  tone !== 'good' && TONE_TEXT[tone]
                )}
              >
                {formatPercent(percent)}
              </span>
              {absolute && (
                <span className="text-xs tabular-nums text-muted-foreground">
                  {absolute}
                </span>
              )}
            </div>
            <Progress
              value={percent ?? 0}
              className={cn('mt-2 h-2', TONE_BAR[tone])}
              aria-label={`${title} ${formatPercent(percent)}`}
            />
            {sub && (
              <p className="mt-2 text-[11px] text-muted-foreground">{sub}</p>
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

type SeriesDef = ThresholdLineSeries

type ChartPanelProps = {
  title: string
  description: string
  series: SeriesDef[]
  data: Record<string, string | number | null>[]
  thresholds?: ThresholdBand[]
  valueFormatter: (v: number) => string
  /** Compact form for the y-axis ticks; defaults to `valueFormatter`. */
  tickFormatter?: (v: number) => string
  isPending: boolean
  errorText?: string | null
  emptyText: string
  /** Caption under the legend (bucket width, retention, exclusions). */
  footer?: string
}

function ChartPanel({
  title,
  description,
  series,
  data,
  thresholds,
  valueFormatter,
  tickFormatter,
  isPending,
  errorText,
  emptyText,
  footer,
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
          <ThresholdLineChart
            data={data}
            xKey="label"
            series={series}
            thresholds={thresholds}
            height={220}
            syncId={CHART_SYNC_ID}
            yTickFormatter={tickFormatter ?? valueFormatter}
            tooltipValueFormatter={valueFormatter}
          />
        )}
        {(series.length > 1 || footer) && (
          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            {series.length > 1 &&
              series.map((s, i) => (
                <span key={s.dataKey} className="flex items-center gap-1.5">
                  <span
                    className="inline-block h-2 w-2 rounded-full"
                    style={{ backgroundColor: seriesLineColor(s.tone, i) }}
                  />
                  {s.label}
                </span>
              ))}
            {footer && <span className="text-[11px]">{footer}</span>}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Docker disk usage
// ---------------------------------------------------------------------------

const SLICE_COLORS = [
  'var(--chart-1)',
  'var(--chart-2)',
  'var(--chart-3)',
  'var(--chart-4)',
]

function DockerDiskUsageCard() {
  const q = useQuery({
    ...nodeDockerDiskUsageGetOptions({
      path: { node_id: CONTROL_PLANE_NODE_ID },
    }),
    staleTime: 5 * 60_000,
    retry: false,
  })
  const usage = q.data
  const reclaimable = usage
    ? DOCKER_USAGE_SLICES.reduce(
        (a, s) => a + (usage[s.key].reclaimable_bytes ?? 0),
        0
      )
    : 0
  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-2 space-y-0 pb-2">
        <div>
          <CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Docker disk usage
          </CardTitle>
          <CardDescription>
            docker system df on the control-plane host
          </CardDescription>
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="h-7 w-7 shrink-0"
          aria-label="Refresh Docker disk usage"
          disabled={q.isFetching}
          onClick={() => void q.refetch()}
        >
          <RefreshCw
            className={cn('h-4 w-4', q.isFetching && 'animate-spin')}
          />
        </Button>
      </CardHeader>
      <CardContent>
        {q.isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-8 w-24" />
            <Skeleton className="h-2 w-full" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        ) : q.isError || !usage ? (
          <div className="space-y-2 text-sm">
            <p className="text-rose-500">docker system df did not answer.</p>
            <p className="text-xs text-muted-foreground">
              {(q.error as { detail?: string } | null)?.detail ??
                'Docker walks every image layer and volume for this figure; it can take up to a minute on a busy host.'}
            </p>
            <Button
              variant="outline"
              size="sm"
              disabled={q.isFetching}
              onClick={() => void q.refetch()}
            >
              Retry
            </Button>
          </div>
        ) : (
          <>
            <div className="flex items-baseline justify-between gap-3">
              <span className="text-2xl font-semibold tracking-tight tabular-nums">
                {formatBytesDecimal(usage.total_bytes)}
              </span>
              <span className="text-xs tabular-nums text-muted-foreground">
                {formatBytesDecimal(reclaimable)} reclaimable
              </span>
            </div>
            <div
              className="mt-2 flex h-2 w-full overflow-hidden rounded-full bg-secondary"
              aria-hidden
            >
              {DOCKER_USAGE_SLICES.map((s, i) => (
                <span
                  key={s.key}
                  style={{
                    width: `${usagePercent(usage[s.key].size_bytes, usage.total_bytes)}%`,
                    backgroundColor: SLICE_COLORS[i],
                  }}
                />
              ))}
            </div>
            <ul className="mt-3 space-y-1.5 text-xs">
              {DOCKER_USAGE_SLICES.map((s, i) => {
                const c = usage[s.key]
                return (
                  <li
                    key={s.key}
                    className="flex items-center justify-between gap-3"
                  >
                    <span className="flex min-w-0 items-center gap-1.5">
                      <span
                        className="inline-block h-2 w-2 shrink-0 rounded-full"
                        style={{ backgroundColor: SLICE_COLORS[i] }}
                      />
                      <span className="truncate">
                        {s.label}
                        <span className="text-muted-foreground">
                          {' '}
                          · {c.active_count}/{c.total_count} in use
                        </span>
                      </span>
                    </span>
                    <span className="shrink-0 tabular-nums">
                      {formatBytesDecimal(c.size_bytes)}
                      <span className="text-muted-foreground">
                        {' '}
                        ·{' '}
                        {formatPercent(
                          usagePercent(c.size_bytes, usage.total_bytes),
                          0
                        )}
                      </span>
                    </span>
                  </li>
                )
              })}
            </ul>
            <p className="mt-2 text-[11px] text-muted-foreground">
              As of {new Date(usage.collected_at).toLocaleTimeString()}
              {usage.api_version ? ` · Docker API ${usage.api_version}` : ''}
            </p>
          </>
        )}
      </CardContent>
    </Card>
  )
}

// ---------------------------------------------------------------------------
// Section
// ---------------------------------------------------------------------------

// The shared chart prints a threshold label in the 24px gutter right of the
// plot, which fits a percentage but not a word; the words go in the card
// description instead.
const bandsOf = (t: UsageThresholds): ThresholdBand[] => [
  { value: t.warn, tone: 'warn', label: `${t.warn}%` },
  { value: t.poor, tone: 'poor', label: `${t.poor}%` },
]
const cpuBands = bandsOf(CPU_THRESHOLDS)
const memoryBands = bandsOf(MEMORY_THRESHOLDS)
const diskBands = bandsOf(DISK_THRESHOLDS)

export function ServerMonitoring() {
  // The window lives in the URL (`?range=6h`) so a link opens on the same
  // view and the browser's back button walks the ranges.
  const [searchParams, setSearchParams] = useSearchParams()
  const rangeParam = searchParams.get('range')
  const range: ProxyRangePreset = isRangePreset(rangeParam) ? rangeParam : '1h'
  const setRange = (next: ProxyRangePreset) =>
    setSearchParams(
      (prev) => {
        const p = new URLSearchParams(prev)
        if (next === '1h') p.delete('range')
        else p.set('range', next)
        return p
      },
      { replace: true }
    )
  // Pause stops the 30 s polling so a sample under investigation stays put.
  const [paused, setPaused] = useState(false)
  const refetchInterval = paused ? false : REFRESH_MS
  const step = STEP_SECONDS[range]
  const showDate = range === '7d'
  const label = (iso: string) => formatProxyTimeLabel(iso, showDate)

  const settings = useSettings()
  const scrapeInterval = settings.data?.monitoring?.scrape_interval_secs ?? 30
  const retentionDays = settings.data?.monitoring?.retention_raw_days ?? 7

  const latest = useQuery({
    ...nodeMetricsGetLatestOptions({ path: { id: CONTROL_PLANE_NODE_ID } }),
    staleTime: 15_000,
    refetchInterval,
    retry: false,
  })
  const cpu = useNodeSeries('node.cpu_percent', range, refetchInterval)
  const memory = useNodeSeries('node.memory_percent', range, refetchInterval)
  const disk = useNodeSeries('node.disk_used_bytes', range, refetchInterval)
  const rx = useNodeSeries(
    'node.network_rx_bytes_total',
    range,
    refetchInterval
  )
  const tx = useNodeSeries(
    'node.network_tx_bytes_total',
    range,
    refetchInterval
  )
  const read = useNodeSeries(
    'node.disk_read_bytes_total',
    range,
    refetchInterval
  )
  const write = useNodeSeries(
    'node.disk_write_bytes_total',
    range,
    refetchInterval
  )

  const snapshot = (latest.data ?? {}) as Record<string, number>
  const g = (k: string): number | null =>
    Number.isFinite(snapshot[k]) ? snapshot[k] : null
  const memTotal = g('node.memory_total_bytes')
  const memUsed = g('node.memory_used_bytes')
  const diskTotal = g('node.disk_total_bytes')
  const diskUsed = g('node.disk_used_bytes')
  const cpuNow = g('node.cpu_percent') ?? lastValue(cpu.data)
  const memPct = g('node.memory_percent') ?? lastValue(memory.data)
  const diskPct =
    g('node.disk_percent') ?? usagePercent(lastValue(disk.data), diskTotal)

  const cpuPeak = peakOf(cpu.data)
  const memPeak = peakOf(memory.data)
  const projection = projectDisk(disk.data, diskTotal)

  const rxRate = toRatePerSecond(rx.data, step)
  const txRate = toRatePerSecond(tx.data, step)
  const readRate = toRatePerSecond(read.data, step)
  const writeRate = toRatePerSecond(write.data, step)

  const usageData = mergeSeries(
    [
      { key: 'cpu', points: cpu.data },
      { key: 'memory', points: memory.data },
      {
        key: 'disk',
        points: diskTotal
          ? disk.data?.map((p) => ({
              time: p.time,
              value: usagePercent(p.value, diskTotal),
            }))
          : undefined,
      },
    ],
    label
  )
  const networkData = mergeSeries(
    [
      { key: 'in', points: rxRate },
      { key: 'out', points: txRate },
    ],
    label
  )
  const blockIoData = mergeSeries(
    [
      { key: 'read', points: readRate },
      { key: 'write', points: writeRate },
    ],
    label
  )

  const lastSampleIso = cpu.data?.[cpu.data.length - 1]?.time
  // Age at the time the series was fetched (refreshed every 30 s), so render
  // stays pure and the number never drifts between two renders of one fetch.
  const ageSeconds =
    lastSampleIso && cpu.dataUpdatedAt
      ? Math.max(0, (cpu.dataUpdatedAt - Date.parse(lastSampleIso)) / 1000)
      : null
  const silent =
    ageSeconds != null && ageSeconds > Math.max(3 * scrapeInterval, 180)

  // Any failed request surfaces at page level with one Retry that refetches
  // everything; the chart it belongs to also says so in place.
  const queries = [latest, cpu, memory, disk, rx, tx, read, write]
  const firstError = queries.find((q) => q.isError)?.error
  const refreshAll = () => {
    for (const q of queries) void q.refetch()
  }

  const bucketCaption = `${step >= 3600 ? `${step / 3600} h` : `${step / 60} min`} buckets · last ${range} · kept ${retentionDays} days`
  const seriesError = (q: { isError: boolean }) =>
    q.isError ? 'Failed to load node metrics' : null

  if (firstError && isMetricsUnavailable(firstError)) {
    return (
      <div className="space-y-6">
        <SectionIntro
          range={range}
          onRange={setRange}
          scrapeInterval={scrapeInterval}
          ageSeconds={null}
          paused={paused}
          onTogglePause={() => setPaused((p) => !p)}
        />
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <AlertTriangle className="h-4 w-4 text-amber-500" />
              Metric collection is not set up
            </CardTitle>
            <CardDescription>
              This server has no metrics store, so the proxy process is not
              sampling this host. With one configured, this page shows CPU,
              memory, disk, Docker disk usage, block I/O and network I/O of the
              control-plane host every {formatAge(scrapeInterval)}, keeps{' '}
              {retentionDays} days of history, and says when the disk will fill.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button asChild size="sm">
              <Link to={METRICS_SETTINGS_PATH}>
                <Settings className="mr-2 h-4 w-4" />
                Metrics monitoring settings
              </Link>
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <SectionIntro
        range={range}
        onRange={setRange}
        scrapeInterval={scrapeInterval}
        ageSeconds={ageSeconds}
        paused={paused}
        onTogglePause={() => setPaused((p) => !p)}
      />

      {firstError && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Could not read node metrics</AlertTitle>
          <AlertDescription className="flex flex-wrap items-center justify-between gap-2">
            <span>
              {(firstError as { detail?: string }).detail ??
                'The metrics store did not answer.'}
            </span>
            <Button variant="outline" size="sm" onClick={refreshAll}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {silent && ageSeconds != null && (
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>No sample for {formatAge(ageSeconds)}</AlertTitle>
          <AlertDescription>
            The node sampler runs inside the proxy process and writes one sample
            every {formatAge(scrapeInterval)}. Check that{' '}
            <code>temps serve</code> is running and that the metrics store
            accepts writes. The values below are the last ones that arrived.
          </AlertDescription>
        </Alert>
      )}

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-4">
        <UsageCard
          title="CPU usage"
          description="All cores of the control-plane host"
          percent={cpuNow}
          absolute={null}
          thresholds={CPU_THRESHOLDS}
          sub={
            cpuPeak
              ? `Peak ${formatPercent(cpuPeak.value)} at ${label(cpuPeak.time)} in the last ${range}`
              : null
          }
          isPending={latest.isPending && cpu.isPending}
        />
        <UsageCard
          title="Memory usage"
          description="Used memory, excluding reclaimable cache"
          percent={memPct}
          absolute={
            memUsed != null && memTotal != null
              ? `${formatBytesBinary(memUsed)} of ${formatBytesBinary(memTotal)}`
              : null
          }
          thresholds={MEMORY_THRESHOLDS}
          sub={
            memPeak
              ? `Peak ${formatPercent(memPeak.value)} at ${label(memPeak.time)} in the last ${range}`
              : null
          }
          isPending={latest.isPending && memory.isPending}
        />
        <UsageCard
          title="Disk space"
          description="Volume under the Temps data directory"
          percent={diskPct}
          absolute={
            diskUsed != null && diskTotal != null
              ? `${formatBytesDecimal(diskUsed)} of ${formatBytesDecimal(diskTotal)}`
              : null
          }
          thresholds={DISK_THRESHOLDS}
          sub={
            diskTotal != null && diskUsed != null
              ? projection && projection.bytesPerDay > 0
                ? `${formatBytesDecimal(diskTotal - diskUsed)} free · growing ${formatBytesDecimal(projection.bytesPerDay)}/day, full in ${formatDays(projection.daysToFull)}`
                : `${formatBytesDecimal(diskTotal - diskUsed)} free · not growing over the last ${range}`
              : null
          }
          isPending={latest.isPending && disk.isPending}
        />
        <DockerDiskUsageCard />
      </div>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        <ChartPanel
          title="CPU usage"
          description={`Share of all cores, per interval · busy above ${CPU_THRESHOLDS.warn}%, saturated above ${CPU_THRESHOLDS.poor}%`}
          series={[{ dataKey: 'cpu', label: 'CPU', tone: 'primary' }]}
          data={usageData}
          thresholds={cpuBands}
          valueFormatter={(v) => formatPercent(v, 0)}
          isPending={cpu.isPending}
          errorText={seriesError(cpu)}
          emptyText="No CPU samples in this window yet"
          footer={bucketCaption}
        />
        <ChartPanel
          title="Memory usage"
          description={
            memTotal != null
              ? `Share of ${formatBytesBinary(memTotal)}, per interval · tight above ${MEMORY_THRESHOLDS.warn}%, OOM risk above ${MEMORY_THRESHOLDS.poor}%`
              : `Share of total memory, per interval · tight above ${MEMORY_THRESHOLDS.warn}%, OOM risk above ${MEMORY_THRESHOLDS.poor}%`
          }
          series={[{ dataKey: 'memory', label: 'Memory', tone: 'primary' }]}
          data={usageData}
          thresholds={memoryBands}
          valueFormatter={(v) => formatPercent(v, 0)}
          isPending={memory.isPending}
          errorText={seriesError(memory)}
          emptyText="No memory samples in this window yet"
          footer={bucketCaption}
        />
        <ChartPanel
          title="Disk space"
          description={
            diskTotal != null
              ? `Share of ${formatBytesDecimal(diskTotal)} used, per interval · tight above ${DISK_THRESHOLDS.warn}%, writes stop near ${DISK_THRESHOLDS.poor}%`
              : `Share of the volume used, per interval · tight above ${DISK_THRESHOLDS.warn}%, writes stop near ${DISK_THRESHOLDS.poor}%`
          }
          series={[{ dataKey: 'disk', label: 'Used', tone: 'primary' }]}
          data={usageData}
          thresholds={diskBands}
          valueFormatter={(v) => formatPercent(v, 0)}
          isPending={disk.isPending}
          errorText={seriesError(disk)}
          emptyText="No disk samples in this window yet"
          footer={bucketCaption}
        />
        <ChartPanel
          title="Network I/O"
          description="Bytes per second on physical interfaces (loopback, bridges and tunnels excluded)"
          series={[
            { dataKey: 'in', label: 'In', tone: 'primary' },
            { dataKey: 'out', label: 'Out', tone: 'good' },
          ]}
          data={networkData}
          valueFormatter={formatBytesPerSecond}
          tickFormatter={formatRateTick}
          isPending={rx.isPending || tx.isPending}
          errorText={seriesError(rx) ?? seriesError(tx)}
          emptyText="No network samples in this window yet"
          footer={bucketCaption}
        />
        <ChartPanel
          title="Block I/O"
          description="Bytes per second read from and written to physical disks (partitions, loop and device-mapper excluded)"
          series={[
            { dataKey: 'read', label: 'Read', tone: 'primary' },
            { dataKey: 'write', label: 'Write', tone: 'good' },
          ]}
          data={blockIoData}
          valueFormatter={formatBytesPerSecond}
          tickFormatter={formatRateTick}
          isPending={read.isPending || write.isPending}
          errorText={seriesError(read) ?? seriesError(write)}
          emptyText="No block I/O samples in this window yet"
          footer={bucketCaption}
        />
      </div>
    </div>
  )
}

function SectionIntro({
  range,
  onRange,
  scrapeInterval,
  ageSeconds,
  paused,
  onTogglePause,
}: {
  range: ProxyRangePreset
  onRange: (r: ProxyRangePreset) => void
  scrapeInterval: number
  ageSeconds: number | null
  paused: boolean
  onTogglePause: () => void
}) {
  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0">
        <h3 className="text-lg font-semibold tracking-tight">Server</h3>
        <p className="text-sm text-muted-foreground">
          Resource usage of the machine running this control plane, sampled
          every {formatAge(scrapeInterval)}
          {ageSeconds != null
            ? ` · last sample ${formatAge(ageSeconds)} ago`
            : ''}
          {paused ? ' · updates paused' : ' · refreshes every 30 s'}. Hover or
          focus a chart and use ← → to read every panel at one instant.
        </p>
      </div>
      <div className="flex shrink-0 flex-wrap items-center gap-1">
        <Button
          variant="outline"
          size="sm"
          aria-pressed={paused}
          onClick={onTogglePause}
          className="mr-2"
        >
          {paused ? (
            <Play className="mr-1.5 h-3.5 w-3.5" />
          ) : (
            <Pause className="mr-1.5 h-3.5 w-3.5" />
          )}
          {paused ? 'Resume' : 'Pause'}
        </Button>
        {PROXY_RANGE_PRESETS.map((opt) => (
          <Button
            key={opt.value}
            variant={range === opt.value ? 'default' : 'outline'}
            size="sm"
            onClick={() => onRange(opt.value)}
          >
            {opt.label}
          </Button>
        ))}
      </div>
    </div>
  )
}
