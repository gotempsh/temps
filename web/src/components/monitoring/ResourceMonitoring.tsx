import {
  getProjectsOptions,
  getTimeBucketStatsOptions,
  getProjectsHealthOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  ChartLegend,
  ChartLegendContent,
} from '@/components/ui/chart'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format, subHours } from 'date-fns'
import { useMemo, useState } from 'react'
import type { DateRange } from 'react-day-picker'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import { Line, LineChart, XAxis, YAxis, CartesianGrid } from 'recharts'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  ArrowUpRight,
  CheckCircle2,
  AlertTriangle,
  XCircle,
  HelpCircle,
} from 'lucide-react'
import { Link } from 'react-router'
import { cn } from '@/lib/utils'

// ── Chart configs ────────────────────────────────────────────────────

const requestsChartConfig = {
  requests: {
    label: 'Requests',
    color: 'var(--chart-1)',
  },
  errors: {
    label: 'Errors',
    color: 'hsl(0 84% 60%)',
  },
} satisfies ChartConfig

const responseTimeChartConfig = {
  response_time: {
    label: 'Avg Response Time (ms)',
    color: 'var(--chart-4)',
  },
} satisfies ChartConfig

// ── Types ────────────────────────────────────────────────────────────

type TimeRange = '1h' | '6h' | '24h' | 'custom'

interface ResolvedRange {
  start: Date
  end: Date
  /** Short, user-facing label like "last 1h" or "Apr 10–16". */
  label: string
  /** TimescaleDB bucket interval matching the window size. */
  bucketInterval: string
}

const PRESET_LABEL: Record<Exclude<TimeRange, 'custom'>, string> = {
  '1h': 'last 1h',
  '6h': 'last 6h',
  '24h': 'last 24h',
}

function pickBucketInterval(hours: number): string {
  if (hours <= 1) return '5 minutes'
  if (hours <= 6) return '30 minutes'
  if (hours <= 48) return '1 hour'
  if (hours <= 24 * 14) return '6 hours'
  return '1 day'
}

function resolveRange(
  timeRange: TimeRange,
  custom: DateRange | undefined,
  now: Date
): ResolvedRange {
  if (timeRange === 'custom' && custom?.from && custom?.to) {
    const hours = (custom.to.getTime() - custom.from.getTime()) / 3_600_000
    const sameDay =
      custom.from.toDateString() === custom.to.toDateString()
    const label = sameDay
      ? `${format(custom.from, 'MMM d, HH:mm')}–${format(custom.to, 'HH:mm')}`
      : `${format(custom.from, 'MMM d')}–${format(custom.to, 'MMM d')}`
    return {
      start: custom.from,
      end: custom.to,
      label,
      bucketInterval: pickBucketInterval(Math.max(hours, 0.25)),
    }
  }
  const preset: Exclude<TimeRange, 'custom'> =
    timeRange === 'custom' ? '24h' : timeRange
  const hours = preset === '1h' ? 1 : preset === '6h' ? 6 : 24
  return {
    start: subHours(now, hours),
    end: now,
    label: PRESET_LABEL[preset],
    bucketInterval: pickBucketInterval(hours),
  }
}

// ── Health status helpers ────────────────────────────────────────────

function getStatusConfig(status: string) {
  switch (status) {
    case 'healthy':
      return {
        icon: CheckCircle2,
        color: 'text-green-600',
        label: 'Healthy',
      }
    case 'degraded':
      return {
        icon: AlertTriangle,
        color: 'text-yellow-600',
        label: 'Degraded',
      }
    case 'down':
      return {
        icon: XCircle,
        color: 'text-red-600',
        label: 'Down',
      }
    default:
      return {
        icon: HelpCircle,
        color: 'text-muted-foreground',
        label: 'Unknown',
      }
  }
}

// ── Project Health Row ──────────────────────────────────────────────

function ProjectHealthRow({
  project,
  health,
}: {
  project: ProjectResponse
  health?: {
    status: string
    total_requests: number
    total_errors: number
    error_rate: number
    avg_response_time_ms: number
  }
}) {
  const status = health?.status ?? 'unknown'
  const config = getStatusConfig(status)
  const StatusIcon = config.icon
  const hasTraffic = health && health.status !== 'unknown'

  return (
    <Link
      to={`/projects/${project.slug}/logs`}
      className="group flex items-center gap-3 px-3 py-2.5 transition-colors hover:bg-muted/50"
    >
      <StatusIcon className={cn('size-4 shrink-0', config.color)} />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{project.name}</p>
        {project.preset && (
          <p className="truncate text-xs text-muted-foreground">
            {project.preset}
          </p>
        )}
      </div>
      {hasTraffic ? (
        <div className="flex shrink-0 items-center gap-4 text-xs tabular-nums">
          <span>
            <span className="font-semibold">
              {health.total_requests.toLocaleString()}
            </span>{' '}
            <span className="text-muted-foreground">req</span>
          </span>
          <span
            className={cn(
              'font-semibold',
              health.error_rate > 5
                ? 'text-red-600'
                : health.error_rate > 1
                  ? 'text-yellow-600'
                  : ''
            )}
          >
            {health.error_rate.toFixed(1)}%
          </span>
          <span>
            <span className="font-semibold">
              {health.avg_response_time_ms.toFixed(0)}
            </span>
            <span className="text-muted-foreground">ms</span>
          </span>
        </div>
      ) : (
        <span className="shrink-0 text-xs text-muted-foreground">
          No traffic
        </span>
      )}
      <ArrowUpRight className="size-3.5 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
    </Link>
  )
}

// ── Simplified Trend Charts ─────────────────────────────────────────

function TrendCharts({
  projectId,
  range,
  hideBots,
}: {
  projectId?: number
  range: ResolvedRange
  hideBots: boolean
}) {
  const { data, isLoading } = useQuery({
    ...getTimeBucketStatsOptions({
      query: {
        start_time: range.start.toISOString(),
        end_time: range.end.toISOString(),
        bucket_interval: range.bucketInterval,
        project_id: projectId,
        // When no project is selected, match the summary cards which only
        // aggregate project-routed requests (project_id IS NOT NULL).
        has_project: projectId === undefined ? true : undefined,
        is_bot: hideBots ? false : undefined,
      },
    }),
    refetchInterval: 30000,
  })

  const chartData = useMemo(() => {
    if (!data?.stats) return []
    return data.stats.map((s) => ({
      time: format(new Date(s.bucket), 'HH:mm'),
      requests: s.request_count,
      errors: s.error_count,
      response_time: Math.round(s.avg_response_time_ms * 10) / 10,
    }))
  }, [data])

  const totals = useMemo(() => {
    if (!data?.stats) return { requests: 0, errors: 0, avgResponse: 0 }
    const requests = data.stats.reduce((s, b) => s + b.request_count, 0)
    const errors = data.stats.reduce((s, b) => s + b.error_count, 0)
    const withTime = data.stats.filter((b) => b.avg_response_time_ms > 0)
    const avgResponse =
      withTime.length > 0
        ? withTime.reduce((s, b) => s + b.avg_response_time_ms, 0) /
          withTime.length
        : 0
    return { requests, errors, avgResponse }
  }, [data])

  if (isLoading) {
    return (
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <Skeleton className="h-[300px] w-full" />
        <Skeleton className="h-[300px] w-full" />
      </div>
    )
  }

  if (!chartData.length) {
    return (
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <Card>
          <CardContent className="flex h-[300px] items-center justify-center text-sm text-muted-foreground">
            No request data for this period
          </CardContent>
        </Card>
        <Card>
          <CardContent className="flex h-[300px] items-center justify-center text-sm text-muted-foreground">
            No response time data for this period
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
      {/* Requests & Errors */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-base">Requests & Errors</CardTitle>
            <div className="flex items-center gap-3 text-sm">
              <span>
                <span className="font-semibold tabular-nums">
                  {totals.requests.toLocaleString()}
                </span>{' '}
                <span className="text-muted-foreground">total</span>
              </span>
              {totals.errors > 0 && (
                <span>
                  <span className="font-semibold text-destructive tabular-nums">
                    {totals.errors.toLocaleString()}
                  </span>{' '}
                  <span className="text-muted-foreground">errors</span>
                </span>
              )}
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <ChartContainer
            config={requestsChartConfig}
            className="h-[240px] w-full"
          >
            <LineChart
              data={chartData}
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
                minTickGap={40}
                tick={{ fontSize: 11 }}
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                tick={{ fontSize: 11 }}
                width={50}
                tickFormatter={(v) => v.toLocaleString()}
              />
              <ChartTooltip content={<ChartTooltipContent />} />
              <ChartLegend content={<ChartLegendContent />} />
              <Line
                dataKey="requests"
                type="monotone"
                stroke="var(--color-requests)"
                strokeWidth={2}
                dot={false}
                activeDot={{ r: 4 }}
              />
              <Line
                dataKey="errors"
                type="monotone"
                stroke="var(--color-errors)"
                strokeWidth={2}
                dot={false}
                activeDot={{ r: 4 }}
              />
            </LineChart>
          </ChartContainer>
        </CardContent>
      </Card>

      {/* Response Time */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-base">Response Time</CardTitle>
            <span className="text-sm">
              <span className="font-semibold tabular-nums">
                {totals.avgResponse.toFixed(0)}
              </span>{' '}
              <span className="text-muted-foreground">ms avg</span>
            </span>
          </div>
        </CardHeader>
        <CardContent>
          <ChartContainer
            config={responseTimeChartConfig}
            className="h-[240px] w-full"
          >
            <LineChart
              data={chartData}
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
                minTickGap={40}
                tick={{ fontSize: 11 }}
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                tick={{ fontSize: 11 }}
                width={50}
                tickFormatter={(v) => `${v}ms`}
              />
              <ChartTooltip
                content={
                  <ChartTooltipContent
                    formatter={(value) => [
                      `${Number(value).toFixed(1)} ms`,
                      'Response Time',
                    ]}
                  />
                }
              />
              <Line
                dataKey="response_time"
                type="monotone"
                stroke="var(--color-response_time)"
                strokeWidth={2}
                dot={false}
                activeDot={{ r: 4 }}
              />
            </LineChart>
          </ChartContainer>
        </CardContent>
      </Card>
    </div>
  )
}

// ── Main page ────────────────────────────────────────────────────────

export function ResourceMonitoring() {
  const [timeRange, setTimeRange] = useState<TimeRange>('24h')
  const [customRange, setCustomRange] = useState<DateRange | undefined>(() => {
    const end = new Date()
    const start = subHours(end, 24)
    return { from: start, to: end }
  })
  const [selectedProjectId, setSelectedProjectId] = useState<string>('all')
  const [hideBots, setHideBots] = useState<boolean>(true)

  const now = useMemo(
    () => {
      const d = new Date()
      d.setSeconds(Math.floor(d.getSeconds() / 30) * 30, 0)
      return d
    },
    [timeRange]
  )

  const range = useMemo(
    () => resolveRange(timeRange, customRange, now),
    [timeRange, customRange, now]
  )

  const { data: projectsData, isLoading: projectsLoading } = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
  })

  const projects = projectsData?.projects ?? []
  const projectIds = useMemo(() => projects.map((p) => p.id), [projects])

  const { data: healthData } = useQuery({
    ...getProjectsHealthOptions({
      query: {
        project_ids: projectIds.join(','),
        start_time: range.start.toISOString(),
        end_time: range.end.toISOString(),
        is_bot: hideBots ? false : undefined,
      },
    }),
    enabled: projectIds.length > 0,
    refetchInterval: timeRange === 'custom' ? false : 30000,
  })

  const healthMap = healthData?.projects ?? {}

  const visibleProjects = useMemo(
    () =>
      projects.filter(
        (p) =>
          selectedProjectId === 'all' ||
          p.id.toString() === selectedProjectId
      ),
    [projects, selectedProjectId]
  )

  return (
    <div className="space-y-6">
      {/* Filter bar */}
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center sm:justify-between">
        <p className="text-sm text-muted-foreground">
          Showing {range.label}
          {selectedProjectId !== 'all' && (
            <> · 1 project</>
          )}
          {hideBots && <> · bots hidden</>}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Select
            value={selectedProjectId}
            onValueChange={setSelectedProjectId}
          >
            <SelectTrigger className="h-8 w-full sm:w-[180px] text-xs">
              <SelectValue placeholder="All projects" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All projects</SelectItem>
              {projects.map((project) => (
                <SelectItem key={project.id} value={project.id.toString()}>
                  {project.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            variant={hideBots ? 'default' : 'outline'}
            size="sm"
            className="h-8 px-3 text-xs"
            onClick={() => setHideBots((v) => !v)}
          >
            {hideBots ? 'Hide bots' : 'Show bots'}
          </Button>
          <div className="flex items-center gap-1 rounded-md border p-0.5">
            {(['1h', '6h', '24h'] as const).map((preset) => (
              <Button
                key={preset}
                variant={timeRange === preset ? 'default' : 'ghost'}
                size="sm"
                className="h-7 px-3 text-xs"
                onClick={() => setTimeRange(preset)}
              >
                {preset}
              </Button>
            ))}
            <Button
              variant={timeRange === 'custom' ? 'default' : 'ghost'}
              size="sm"
              className="h-7 px-3 text-xs"
              onClick={() => setTimeRange('custom')}
            >
              Custom
            </Button>
          </div>
          {timeRange === 'custom' && (
            <DateRangePicker
              date={customRange}
              onDateChange={setCustomRange}
              showTime
              className="w-full sm:w-[300px]"
            />
          )}
        </div>
      </div>

      {projectsLoading ? (
        <div className="space-y-6">
          <Skeleton className="h-[160px] w-full" />
          <Skeleton className="h-[300px] w-full" />
        </div>
      ) : projects.length === 0 ? (
        <p className="rounded-md border border-dashed px-4 py-10 text-center text-sm text-muted-foreground">
          No projects found. Create a project to start monitoring.
        </p>
      ) : (
        <div className="space-y-6">
          {/* ── Project Health Rows ── */}
          <Card>
            <ul role="list" className="divide-y divide-gray-950/5">
              {visibleProjects.map((project) => (
                <li key={project.id}>
                  <ProjectHealthRow
                    project={project}
                    health={healthMap[project.id.toString()]}
                  />
                </li>
              ))}
            </ul>
          </Card>

          {/* ── Trend Charts ── */}
          <TrendCharts
            projectId={
              selectedProjectId !== 'all'
                ? parseInt(selectedProjectId, 10)
                : undefined
            }
            range={range}
            hideBots={hideBots}
          />
        </div>
      )}
    </div>
  )
}
