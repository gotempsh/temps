// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
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
import {
  getIngestErrorsOptions,
  getPipelineHistoryOptions,
  getPipelineStatsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  IngestErrorSummary,
  PipelineHistoryResponse,
} from '@/api/client/types.gen'
import {
  formatChartTick,
  formatChartTooltipLabel,
  TOOLTIP_CONTENT_STYLE,
  TOOLTIP_LABEL_STYLE,
} from '@/lib/chart-tooltip'
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  AlertCircle,
  AlertTriangle,
  ArrowRight,
  Activity,
  ChevronDown,
  CheckCircle2,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router'
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
// Trend chart
// ---------------------------------------------------------------------------

/** Presets accepted by `GET /otel/pipeline-history` (`range_to_step` server-side). */
const RANGE_PRESETS = [
  { value: '1h', label: 'Last hour' },
  { value: '6h', label: 'Last 6 hours' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
] as const

type RangePreset = (typeof RANGE_PRESETS)[number]['value']

/** Multi-day ranges get a date on the x-axis; same-time points would collide. */
const RANGES_SHOWING_DATE: RangePreset[] = ['7d']

type TrendSeriesDef = {
  /** Metric name as published by the sampler (`OTEL_PIPELINE_METRIC_NAMES`). */
  metric: string
  label: string
  color: string
}

type TrendPanelDef = {
  title: string
  description: string
  series: TrendSeriesDef[]
}

// Colours mirror the existing convention on this page and in ProxyMetrics:
// blue = volume in, green = success, amber = warning, red = loss.
const TREND_PANELS: TrendPanelDef[] = [
  {
    title: 'Traces (spans)',
    description: 'Spans received, stored and dropped per sample',
    series: [
      { metric: 'otel.spans_received', label: 'Received', color: '#2563eb' },
      { metric: 'otel.spans_stored', label: 'Stored', color: '#16a34a' },
      { metric: 'otel.spans_dropped', label: 'Dropped', color: '#dc2626' },
    ],
  },
  {
    title: 'Metrics',
    description: 'Metric points received, stored and dropped per sample',
    series: [
      { metric: 'otel.metrics_received', label: 'Received', color: '#2563eb' },
      { metric: 'otel.metrics_stored', label: 'Stored', color: '#16a34a' },
      { metric: 'otel.metrics_dropped', label: 'Dropped', color: '#dc2626' },
    ],
  },
  {
    title: 'Logs',
    description: 'Log records received, persisted to DB and S3, and dropped',
    series: [
      { metric: 'otel.logs_received', label: 'Received', color: '#2563eb' },
      { metric: 'otel.logs_stored_db', label: 'Stored (DB)', color: '#16a34a' },
      {
        metric: 'otel.logs_stored_s3',
        label: 'Archived (S3)',
        color: '#7c3aed',
      },
      { metric: 'otel.logs_dropped', label: 'Dropped', color: '#dc2626' },
    ],
  },
  {
    title: 'Rejections & errors',
    description: 'Requests turned away, and writes that failed after retries',
    series: [
      {
        metric: 'otel.rate_limited_requests',
        label: 'Rate limited',
        color: '#d97706',
      },
      {
        metric: 'otel.quota_exceeded_requests',
        label: 'Quota exceeded',
        color: '#7c3aed',
      },
      {
        metric: 'otel.ingest_errors',
        label: 'Ingest errors',
        color: '#dc2626',
      },
    ],
  },
]

/** A recharts row: epoch-ms timestamp plus one key per metric in the panel. */
type TrendRow = { ts: number } & Record<string, number>

/**
 * Pivot the API's series-of-points into recharts' row-per-timestamp shape.
 *
 * Every series shares the same server-derived bucket grid, so timestamps align
 * and a missing sample simply leaves that key absent for the row (recharts
 * renders a gap rather than a false zero).
 */
function buildTrendRows(
  history: PipelineHistoryResponse | undefined,
  metrics: string[]
): TrendRow[] {
  if (!history) return []
  const byTs = new Map<number, TrendRow>()

  for (const series of history.series) {
    if (!metrics.includes(series.name)) continue
    for (const point of series.points) {
      const ts = new Date(point.time).getTime()
      if (Number.isNaN(ts)) continue
      const row = byTs.get(ts) ?? ({ ts } as TrendRow)
      row[series.name] = point.value
      byTs.set(ts, row)
    }
  }

  return [...byTs.values()].sort((a, b) => a.ts - b.ts)
}

function hasAnyTrendData(rows: TrendRow[], metrics: string[]): boolean {
  return rows.some((row) => metrics.some((m) => (row[m] ?? 0) > 0))
}

function TrendPanel({
  panel,
  history,
  isLoading,
  showDate,
}: {
  panel: TrendPanelDef
  history: PipelineHistoryResponse | undefined
  isLoading: boolean
  showDate: boolean
}) {
  const metrics = useMemo(
    () => panel.series.map((s) => s.metric),
    [panel.series]
  )
  const rows = useMemo(
    () => buildTrendRows(history, metrics),
    [history, metrics]
  )
  const isFlat = !isLoading && !hasAnyTrendData(rows, metrics)

  return (
    <div className="rounded-lg border bg-card p-4">
      <h3 className="text-sm font-medium">{panel.title}</h3>
      <p className="mt-0.5 text-xs text-muted-foreground">
        {panel.description}
      </p>

      {isLoading ? (
        <Skeleton className="mt-3 h-[180px] w-full" />
      ) : (
        <div className="relative mt-3 h-[180px] w-full">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart
              data={rows}
              margin={{ top: 4, right: 8, bottom: 0, left: -18 }}
            >
              <CartesianGrid strokeDasharray="3 3" stroke="var(--border)" />
              <XAxis
                dataKey="ts"
                type="number"
                domain={['dataMin', 'dataMax']}
                scale="time"
                tickFormatter={
                  showDate ? formatChartTooltipLabel : formatChartTick
                }
                tick={{ fontSize: 10, fill: 'var(--muted-foreground)' }}
                minTickGap={28}
              />
              <YAxis
                allowDecimals={false}
                tick={{ fontSize: 10, fill: 'var(--muted-foreground)' }}
                width={44}
              />
              <Tooltip
                contentStyle={TOOLTIP_CONTENT_STYLE}
                labelStyle={TOOLTIP_LABEL_STYLE}
                labelFormatter={(l) => formatChartTooltipLabel(Number(l))}
              />
              {panel.series.map((s) => (
                <Line
                  key={s.metric}
                  type="monotone"
                  dataKey={s.metric}
                  name={s.label}
                  stroke={s.color}
                  strokeWidth={1.75}
                  dot={false}
                  isAnimationActive={false}
                />
              ))}
            </LineChart>
          </ResponsiveContainer>

          {isFlat && (
            <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
              <span className="rounded bg-background/80 px-2 py-1 text-xs text-muted-foreground">
                No activity in this window
              </span>
            </div>
          )}
        </div>
      )}

      <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1">
        {panel.series.map((s) => (
          <span
            key={s.metric}
            className="inline-flex items-center gap-1 text-xs text-muted-foreground"
          >
            <span
              className="inline-block h-2 w-2 rounded-full"
              style={{ backgroundColor: s.color }}
            />
            {s.label}
          </span>
        ))}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Ingest error drill-down
// ---------------------------------------------------------------------------

/**
 * Turn `clickhouse_network` into `ClickHouse network` for display.
 *
 * Product names are cased explicitly — a naive capitalize renders "Clickhouse"
 * and "Postgres conn", which reads as a typo in an operator-facing table.
 */
const ERROR_CLASS_WORDS: Record<string, string> = {
  clickhouse: 'ClickHouse',
  postgres: 'Postgres',
  conn: 'connection',
  s3: 'S3',
  io: 'I/O',
}

function humanizeErrorClass(errorClass: string): string {
  const words = errorClass
    .split('_')
    .map((w) => ERROR_CLASS_WORDS[w] ?? w)
    .join(' ')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

function formatRelative(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return formatDistanceToNow(date, { addSuffix: true })
}

function IngestErrorRow({ entry }: { entry: IngestErrorSummary }) {
  return (
    <TableRow>
      <TableCell className="font-medium capitalize">
        {entry.signal_type}
      </TableCell>
      <TableCell>
        <Badge variant="outline" className="font-mono text-xs">
          {humanizeErrorClass(entry.error_class)}
        </Badge>
      </TableCell>
      <TableCell className="hidden md:table-cell">
        <span
          className="block max-w-[420px] truncate text-xs text-muted-foreground"
          title={entry.sample_message}
        >
          {entry.sample_message}
        </span>
      </TableCell>
      <TableCell className="text-right tabular-nums">
        {entry.count.toLocaleString()}
      </TableCell>
      <TableCell className="hidden whitespace-nowrap text-right text-xs text-muted-foreground sm:table-cell">
        {formatRelative(entry.last_seen)}
      </TableCell>
    </TableRow>
  )
}

function IngestErrorList({
  entries,
  isLoading,
  isError,
}: {
  entries: IngestErrorSummary[]
  isLoading: boolean
  isError: boolean
}) {
  if (isLoading) {
    return (
      <div className="space-y-2">
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
      </div>
    )
  }

  if (isError) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Could not load ingest errors</AlertTitle>
        <AlertDescription>
          The failure reasons could not be fetched. The counters above are still
          accurate.
        </AlertDescription>
      </Alert>
    )
  }

  if (entries.length === 0) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
        <CheckCircle2 className="h-4 w-4 text-green-600 dark:text-green-500" />
        No ingest errors recorded in the last 7 days.
      </div>
    )
  }

  return (
    <div className="overflow-x-auto">
      <Table className="min-w-[420px]">
        <TableHeader>
          <TableRow>
            <TableHead>Signal</TableHead>
            <TableHead>Reason</TableHead>
            <TableHead className="hidden md:table-cell">
              Sample message
            </TableHead>
            <TableHead className="text-right">Count</TableHead>
            <TableHead className="hidden text-right sm:table-cell">
              Last seen
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.map((entry) => (
            <IngestErrorRow
              key={`${entry.signal_type}:${entry.error_class}`}
              entry={entry}
            />
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

function StatCard({
  label,
  value,
  warn,
  description,
}: {
  label: string
  value: number | undefined
  warn?: boolean
  description?: string
}) {
  return (
    <div
      className={`rounded-lg border p-4 ${warn && value ? 'border-amber-400 bg-amber-50 dark:bg-amber-950/30' : 'bg-card'}`}
    >
      <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
        {label}
      </p>
      {value === undefined ? (
        <Skeleton className="mt-2 h-7 w-20" />
      ) : (
        <p
          className={`mt-1 text-2xl font-semibold tabular-nums ${warn && value > 0 ? 'text-amber-600 dark:text-amber-400' : ''}`}
        >
          {value.toLocaleString()}
        </p>
      )}
      {description && (
        <p className="mt-1 text-xs text-muted-foreground">{description}</p>
      )}
    </div>
  )
}

function SignalSection({
  label,
  received,
  stored,
  dropped,
  isLoading,
}: {
  label: string
  received: number | undefined
  stored: number | undefined
  dropped: number | undefined
  isLoading: boolean
}) {
  const droppedCount = dropped ?? 0
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <h3 className="text-sm font-medium">{label}</h3>
        {!isLoading && droppedCount > 0 && (
          <Badge variant="destructive" className="text-xs">
            {droppedCount.toLocaleString()} dropped
          </Badge>
        )}
      </div>
      <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
        <StatCard
          label="Received"
          value={isLoading ? undefined : received}
          description="Total ingest requests"
        />
        <StatCard
          label="Stored"
          value={isLoading ? undefined : stored}
          description="Successfully persisted"
        />
        <StatCard
          label="Dropped"
          value={isLoading ? undefined : dropped}
          description="Failed to store"
        />
      </div>
    </div>
  )
}

export function OtelPipelineStatusPage() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'OTel Pipeline Status' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('OTel Pipeline Status')

  const [range, setRange] = useState<RangePreset>('24h')
  const [errorsOpen, setErrorsOpen] = useState(false)

  const { data, isLoading, error } = useQuery({
    ...getPipelineStatsOptions({ cache: 'no-store' }),
    // Matches NodesPage's status-tile cadence: frequent enough that an
    // operator watching this page after a rejection spike sees it clear
    // without a manual refresh, cheap enough for a page that's rarely open.
    refetchInterval: 30_000,
  })

  // The sampler only writes every 60s, so polling the history faster than
  // that would re-fetch identical buckets.
  const {
    data: history,
    isLoading: historyLoading,
    error: historyError,
  } = useQuery({
    ...getPipelineHistoryOptions({ query: { range } }),
    refetchInterval: 60_000,
  })

  const {
    data: ingestErrors,
    isLoading: ingestErrorsLoading,
    isError: ingestErrorsFailed,
  } = useQuery({
    ...getIngestErrorsOptions({ query: { limit: 100 } }),
    refetchInterval: 60_000,
  })

  const stats = data?.stats
  const errorEntries = ingestErrors?.errors ?? []
  const showDate = RANGES_SHOWING_DATE.includes(range)
  const sampleIntervalSeconds = history?.sample_interval_seconds ?? 60

  const rateLimited = stats?.rate_limited_requests ?? 0
  const quotaExceeded = stats?.quota_exceeded_requests ?? 0
  const hasRejections = !isLoading && (rateLimited > 0 || quotaExceeded > 0)

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Failed to load pipeline stats</AlertTitle>
        <AlertDescription>
          Could not fetch OTel pipeline statistics. The server may be
          unavailable or you may not have permission.
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <Activity className="h-5 w-5 text-muted-foreground" />
          <h1 className="text-xl font-semibold">OTel Pipeline Status</h1>
        </div>
        <p className="text-sm text-muted-foreground">
          Throughput and failures for the OTLP ingest pipeline. The trend charts
          cover the selected window; the counters below are cumulative since the
          last server restart. All counters are written to the metrics store
          every {sampleIntervalSeconds}&nbsp;s and can trigger alarms.
        </p>
      </div>

      {/* Trend over time — the cumulative counters below can't distinguish a
          past incident that recovered from an ongoing bleed. */}
      <Card>
        <CardHeader className="pb-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <div>
              <CardTitle className="text-base">Pipeline trend</CardTitle>
              <CardDescription>
                Sampled every {sampleIntervalSeconds}&nbsp;s. Each point is the
                average per-sample count for its bucket, so a value of 10 means
                ~10 per {sampleIntervalSeconds}&nbsp;s — not 10 in total.
              </CardDescription>
            </div>
            <Select
              value={range}
              onValueChange={(v) => setRange(v as RangePreset)}
            >
              <SelectTrigger
                className="w-full sm:w-[160px]"
                aria-label="Time window"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {RANGE_PRESETS.map((preset) => (
                  <SelectItem key={preset.value} value={preset.value}>
                    {preset.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </CardHeader>
        <CardContent>
          {historyError ? (
            <Alert>
              <AlertTriangle className="h-4 w-4 text-amber-500" />
              <AlertTitle>History unavailable</AlertTitle>
              <AlertDescription>
                {(historyError as any)?.status === 503 ? (
                  <>
                    Pipeline history is recorded by the metrics store, which is
                    not enabled on this server — the live counters below are
                    unaffected. Enable it from{' '}
                    <Link
                      to="/settings/metrics-monitoring"
                      className="inline-flex items-center gap-1 font-medium underline underline-offset-2"
                    >
                      Metrics &amp; monitoring settings
                      <ArrowRight className="h-3 w-3" />
                    </Link>
                    .
                  </>
                ) : (
                  'Pipeline history could not be loaded. The live counters below are unaffected.'
                )}
              </AlertDescription>
            </Alert>
          ) : (
            <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
              {TREND_PANELS.map((panel) => (
                <TrendPanel
                  key={panel.title}
                  panel={panel}
                  history={history}
                  isLoading={historyLoading}
                  showDate={showDate}
                />
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      {/* Rejection counters — always shown first since they're the point of this page */}
      <Card className={hasRejections ? 'border-amber-400' : undefined}>
        <CardHeader className="pb-3">
          <div className="flex items-center gap-2">
            <CardTitle className="text-base">Rejected requests</CardTitle>
            {hasRejections && (
              <AlertTriangle className="h-4 w-4 text-amber-500" />
            )}
          </div>
          <CardDescription>
            Ingest requests turned away since the server started. Non-zero
            values mean projects are hitting their quotas or rate limits.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
            <StatCard
              label="Rate limited (429)"
              value={isLoading ? undefined : rateLimited}
              warn
              description="otel.rate_limited_requests"
            />
            <StatCard
              label="Quota exceeded (413)"
              value={isLoading ? undefined : quotaExceeded}
              warn
              description="otel.quota_exceeded_requests"
            />
          </div>

          {hasRejections && (
            <Alert className="mt-4" variant="default">
              <AlertTriangle className="h-4 w-4 text-amber-500" />
              <AlertTitle>Rejections detected</AlertTitle>
              <AlertDescription className="flex items-center gap-2">
                Projects are being rate-limited or have exceeded their storage
                quota. Check the{' '}
                <Link
                  to="/monitoring/alarms"
                  className="inline-flex items-center gap-1 font-medium underline underline-offset-2"
                >
                  Alarms page
                  <ArrowRight className="h-3 w-3" />
                </Link>{' '}
                to see whether the OtelRateLimited alarm is firing.
              </AlertDescription>
            </Alert>
          )}

          {!hasRejections && !isLoading && (
            <p className="mt-3 text-xs text-muted-foreground">
              No rejections recorded.{' '}
              <Link
                to="/monitoring/alarms"
                className="inline-flex items-center gap-1 underline underline-offset-2"
              >
                View alarms
                <ArrowRight className="h-3 w-3" />
              </Link>
            </p>
          )}
        </CardContent>
      </Card>

      {/* Per-signal pipeline health */}
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Pipeline throughput</CardTitle>
          <CardDescription>
            Received vs. stored vs. dropped counts per signal type since the
            last server restart.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <SignalSection
            label="Traces (spans)"
            received={stats?.spans_received}
            stored={stats?.spans_stored}
            dropped={stats?.spans_dropped}
            isLoading={isLoading}
          />
          <SignalSection
            label="Metrics"
            received={stats?.metrics_received}
            stored={stats?.metrics_stored}
            dropped={stats?.metrics_dropped}
            isLoading={isLoading}
          />
          <SignalSection
            label="Logs"
            received={stats?.logs_received}
            stored={stats?.logs_stored_db}
            dropped={stats?.logs_dropped}
            isLoading={isLoading}
          />

          {/* Ingest errors: the lifetime count stays as the summary, but the
              reason each batch was dropped is what an operator actually needs,
              so the count now expands into the grouped failure list. */}
          <div className="border-t pt-4">
            <Collapsible open={errorsOpen} onOpenChange={setErrorsOpen}>
              <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex items-center gap-2">
                  <h3 className="text-sm font-medium">Ingest errors</h3>
                  {!isLoading && (stats?.ingest_errors ?? 0) > 0 && (
                    <Badge variant="destructive" className="text-xs">
                      {(stats?.ingest_errors ?? 0).toLocaleString()} errors
                    </Badge>
                  )}
                </div>
                <CollapsibleTrigger asChild>
                  <Button
                    variant="outline"
                    size="sm"
                    className="w-full justify-center sm:w-auto"
                  >
                    <ChevronDown
                      className={`h-4 w-4 transition-transform ${errorsOpen ? 'rotate-180' : ''}`}
                    />
                    <span className="ml-1">
                      {errorsOpen ? 'Hide' : 'Show'} failure reasons
                      {!ingestErrorsLoading &&
                        errorEntries.length > 0 &&
                        ` (${errorEntries.length})`}
                    </span>
                  </Button>
                </CollapsibleTrigger>
              </div>

              <div className="mt-2 grid grid-cols-2 md:grid-cols-4 gap-3">
                <StatCard
                  label="Ingest errors"
                  value={isLoading ? undefined : stats?.ingest_errors}
                  description="Storage writes that failed after retries"
                />
              </div>

              <CollapsibleContent className="mt-4">
                <IngestErrorList
                  entries={errorEntries}
                  isLoading={ingestErrorsLoading}
                  isError={ingestErrorsFailed}
                />
                <p className="mt-2 text-xs text-muted-foreground">
                  Grouped by signal and failure reason, newest first. Recorded
                  only after a write has exhausted its retries, so an entry here
                  means data was actually lost.
                </p>
              </CollapsibleContent>
            </Collapsible>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
