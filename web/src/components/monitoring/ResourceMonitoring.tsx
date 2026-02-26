import {
  getEnvironmentsOptions,
  getProjectsOptions,
  getTimeBucketStatsOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  ContainerInfoResponse,
  EnvironmentResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
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
import { useCallback, useMemo, useState } from 'react'
import { Line, LineChart, XAxis, YAxis, CartesianGrid } from 'recharts'
import {
  EnvironmentMetricsCharts,
  type AggregatedMetrics,
} from './EnvironmentMetricsCard'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Activity, Wifi, WifiOff } from 'lucide-react'
import { Badge } from '@/components/ui/badge'

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

type TimeRange = '1h' | '6h' | '24h'

type StatusCodeFilter = 'all' | '2xx' | '3xx' | '4xx' | '5xx' | string

const STATUS_CODE_OPTIONS: { value: StatusCodeFilter; label: string }[] = [
  { value: 'all', label: 'All status codes' },
  { value: '2xx', label: '2xx Success' },
  { value: '3xx', label: '3xx Redirect' },
  { value: '4xx', label: '4xx Client Error' },
  { value: '5xx', label: '5xx Server Error' },
  { value: '200', label: '200 OK' },
  { value: '201', label: '201 Created' },
  { value: '301', label: '301 Moved' },
  { value: '302', label: '302 Found' },
  { value: '304', label: '304 Not Modified' },
  { value: '400', label: '400 Bad Request' },
  { value: '401', label: '401 Unauthorized' },
  { value: '403', label: '403 Forbidden' },
  { value: '404', label: '404 Not Found' },
  { value: '429', label: '429 Too Many Requests' },
  { value: '500', label: '500 Internal Server Error' },
  { value: '502', label: '502 Bad Gateway' },
  { value: '503', label: '503 Service Unavailable' },
]

// ── Requests Line Chart ──────────────────────────────────────────────

function RequestsLineChart({
  projectId,
  timeRange,
  statusCodeFilter,
}: {
  projectId?: number
  timeRange: TimeRange
  statusCodeFilter: StatusCodeFilter
}) {
  const { startDate, endDate, bucketInterval } = useMemo(() => {
    const end = new Date()
    const hours = timeRange === '1h' ? 1 : timeRange === '6h' ? 6 : 24
    const start = subHours(end, hours)
    const interval =
      timeRange === '1h'
        ? '5 minutes'
        : timeRange === '6h'
          ? '30 minutes'
          : '1 hour'
    return {
      startDate: start.toISOString(),
      endDate: end.toISOString(),
      bucketInterval: interval,
    }
  }, [timeRange])

  const statusCodeQuery = useMemo(() => {
    if (statusCodeFilter === 'all') return {}
    if (/^\d{3}$/.test(statusCodeFilter)) {
      return { status_code: parseInt(statusCodeFilter, 10) }
    }
    if (/^\dxx$/.test(statusCodeFilter)) {
      return { status_code_class: statusCodeFilter }
    }
    return {}
  }, [statusCodeFilter])

  const { data, isLoading } = useQuery({
    ...getTimeBucketStatsOptions({
      query: {
        start_time: startDate,
        end_time: endDate,
        bucket_interval: bucketInterval,
        project_id: projectId,
        ...statusCodeQuery,
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
          <CardContent className="flex items-center justify-center h-[300px] text-sm text-muted-foreground">
            No request data for this period
          </CardContent>
        </Card>
        <Card>
          <CardContent className="flex items-center justify-center h-[300px] text-sm text-muted-foreground">
            No response time data for this period
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
      {/* Requests over time */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-base">Requests</CardTitle>
            <div className="flex items-center gap-3 text-sm">
              <span>
                <span className="font-semibold">
                  {totals.requests.toLocaleString()}
                </span>{' '}
                <span className="text-muted-foreground">total</span>
              </span>
              {totals.errors > 0 && (
                <span>
                  <span className="font-semibold text-destructive">
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

      {/* Response time */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-base">Response Time</CardTitle>
            <span className="text-sm">
              <span className="font-semibold">
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

// ── Per-environment CPU/Memory charts ────────────────────────────────

function EnvironmentSection({
  project,
  environment,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
}) {
  const { data: containerList } = useQuery({
    ...listContainersOptions({
      path: {
        project_id: project.id,
        environment_id: environment.id,
      },
    }),
    enabled: project.preset !== 'static',
  })

  const containers: ContainerInfoResponse[] = containerList?.containers ?? []

  const [liveMetrics, setLiveMetrics] = useState<AggregatedMetrics | null>(
    null
  )

  const handleMetricsUpdate = useCallback(
    (metrics: AggregatedMetrics | null) => {
      setLiveMetrics(metrics)
    },
    []
  )

  const hasRunningContainers = containers.some((c) => c.status === 'running')

  return (
    <Card>
      <CardHeader className="pb-2">
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="text-base">
              {project.name}{' '}
              <span className="text-muted-foreground font-normal">
                / {environment.name}
              </span>
            </CardTitle>
          </div>
          <div className="flex items-center gap-3">
            {liveMetrics && (
              <div className="flex items-center gap-3 text-sm">
                <span>
                  CPU{' '}
                  <span className="font-semibold">
                    {liveMetrics.cpu.toFixed(1)}%
                  </span>
                </span>
                <span>
                  Mem{' '}
                  <span className="font-semibold">
                    {liveMetrics.memoryMb.toFixed(0)} MB
                  </span>
                  <span className="text-muted-foreground ml-1">
                    ({liveMetrics.memoryPercent.toFixed(0)}%)
                  </span>
                </span>
              </div>
            )}
            {hasRunningContainers ? (
              <Badge
                variant="outline"
                className="text-green-600 border-green-600/30 gap-1"
              >
                <Wifi className="h-3 w-3" />
                Live
              </Badge>
            ) : (
              <Badge variant="secondary" className="gap-1">
                <WifiOff className="h-3 w-3" />
                Offline
              </Badge>
            )}
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {project.preset === 'static' ? (
          <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
            Static projects don't have container metrics
          </div>
        ) : (
          <EnvironmentMetricsCharts
            projectId={project.id}
            environment={environment}
            containers={containers}
            onMetricsUpdate={handleMetricsUpdate}
          />
        )}
      </CardContent>
    </Card>
  )
}

function ProjectEnvironments({ project }: { project: ProjectResponse }) {
  const { data: environments, isLoading } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
  })

  if (isLoading) {
    return <Skeleton className="h-[300px] w-full" />
  }

  if (!environments || environments.length === 0) {
    return null
  }

  const activeEnvs = environments.filter(
    (env) => env.current_deployment_id != null
  )

  if (activeEnvs.length === 0) {
    return null
  }

  return (
    <>
      {activeEnvs.map((env) => (
        <EnvironmentSection
          key={env.id}
          project={project}
          environment={env}
        />
      ))}
    </>
  )
}

// ── Main page ────────────────────────────────────────────────────────

export function ResourceMonitoring() {
  const [timeRange, setTimeRange] = useState<TimeRange>('1h')
  const [selectedProjectId, setSelectedProjectId] = useState<string>('all')
  const [statusCodeFilter, setStatusCodeFilter] =
    useState<StatusCodeFilter>('all')

  const { data: projectsData, isLoading: projectsLoading } = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
  })

  const projects = projectsData?.projects ?? []
  const serverProjects = useMemo(
    () => projects.filter((p) => p.preset !== 'static'),
    [projects]
  )

  const filteredProjects = useMemo(() => {
    if (selectedProjectId === 'all') return serverProjects
    return serverProjects.filter(
      (p) => p.id.toString() === selectedProjectId
    )
  }, [serverProjects, selectedProjectId])

  return (
    <div className="space-y-6">
      {/* Header + Controls */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="text-lg font-medium">Resource Monitoring</h3>
          <p className="text-sm text-muted-foreground">
            Requests, CPU, and memory across projects and environments
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Select
            value={selectedProjectId}
            onValueChange={setSelectedProjectId}
          >
            <SelectTrigger className="w-[180px]">
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
          <Select
            value={statusCodeFilter}
            onValueChange={setStatusCodeFilter}
          >
            <SelectTrigger className="w-[200px]">
              <SelectValue placeholder="All status codes" />
            </SelectTrigger>
            <SelectContent>
              {STATUS_CODE_OPTIONS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <div className="flex items-center gap-1 rounded-md border p-0.5">
            {(['1h', '6h', '24h'] as const).map((range) => (
              <Button
                key={range}
                variant={timeRange === range ? 'default' : 'ghost'}
                size="sm"
                className="h-7 px-3 text-xs"
                onClick={() => setTimeRange(range)}
              >
                {range}
              </Button>
            ))}
          </div>
        </div>
      </div>

      {projectsLoading ? (
        <div className="space-y-6">
          <Skeleton className="h-[340px] w-full" />
          <Skeleton className="h-[240px] w-full" />
          <Skeleton className="h-[500px] w-full" />
        </div>
      ) : projects.length === 0 ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <Activity className="h-10 w-10 text-muted-foreground mb-4" />
            <p className="text-sm text-muted-foreground">
              No projects found. Create a project to start monitoring.
            </p>
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-6">
          {/* ── HTTP Requests Charts ── */}
          <RequestsLineChart
            projectId={
              selectedProjectId !== 'all'
                ? parseInt(selectedProjectId, 10)
                : undefined
            }
            timeRange={timeRange}
            statusCodeFilter={statusCodeFilter}
          />

          {/* ── CPU / Memory per project-environment ── */}
          {filteredProjects.map((project) => (
            <ProjectEnvironments key={project.id} project={project} />
          ))}
        </div>
      )}
    </div>
  )
}
