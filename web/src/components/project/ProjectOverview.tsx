// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ApiRouteEntry,
  DeploymentResponse,
  ErrorGroupResponse,
  EventTimeline,
  ProjectResponse,
} from '@/api/client'
import {
  containerMetricsGetHistoryOptions,
  getApiRoutesOptions,
  getApiTimeseriesOptions,
  getEnvironmentsOptions,
  getErrorDashboardStatsOptions,
  getHourlyVisitsOptions,
  getUniqueCountsOptions,
  hasAnalyticsEventsOptions,
  listContainerHistoryOptions,
  listErrorGroupsOptions,
  queryTraceSummariesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ThresholdLineChart } from '@/components/charts/threshold-line-chart'
import { VisitorSparkline } from '@/components/dashboard/VisitorSparkline'
import { LastDeployment } from '@/components/deployments/LastDeployment'
import { RecentDeployments } from '@/components/deployments/RecentDeployments'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { buildProjectAnalyticsCountRequest } from '@/lib/project-analytics-summary'
import { useQueries, useQuery } from '@tanstack/react-query'
import { format, subDays } from 'date-fns'
import { ArrowRight, Sparkles } from 'lucide-react'
import { useMemo } from 'react'
import { Link } from 'react-router'
import { PROJECT_TOUR_EVENT } from './ProjectTour'

interface ProjectOverviewProps {
  project: ProjectResponse
  lastDeployment?: DeploymentResponse
}

export function ProjectOverview({
  project,
  lastDeployment,
}: ProjectOverviewProps) {
  const { startDate, endDate } = useMemo(
    () => ({
      startDate: subDays(new Date(), 1),
      endDate: new Date(),
    }),
    []
  )

  const analyticsCapabilityQuery = useQuery({
    ...hasAnalyticsEventsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })
  const analyticsEnabled = analyticsCapabilityQuery.data?.has_events === true
  const analyticsWindow = {
    start_date: startDate.toISOString(),
    end_date: endDate.toISOString(),
  }

  const visitorsQuery = useQuery({
    ...getUniqueCountsOptions({
      ...buildProjectAnalyticsCountRequest(
        project.id,
        analyticsWindow,
        'visitors'
      ),
    }),
    enabled: analyticsEnabled,
  })

  const pageViewsQuery = useQuery({
    ...getUniqueCountsOptions({
      ...buildProjectAnalyticsCountRequest(
        project.id,
        analyticsWindow,
        'page_views'
      ),
    }),
    enabled: analyticsEnabled,
  })

  const returningVisitorsQuery = useQuery({
    ...getUniqueCountsOptions({
      ...buildProjectAnalyticsCountRequest(
        project.id,
        analyticsWindow,
        'returning_visitors'
      ),
    }),
    enabled: analyticsEnabled,
  })

  const hourlyVisitorsQuery = useQuery({
    ...getHourlyVisitsOptions({
      path: { project_id: project.id },
      query: { ...analyticsWindow, aggregation_level: 'visitors' },
    }),
    enabled: analyticsEnabled,
  })

  const {
    data: errorStats,
    isPending: isLoadingErrors,
    isError: hasErrorStatsError,
  } = useQuery({
    ...getErrorDashboardStatsOptions({
      query: {
        start_time: startDate.toISOString(),
        end_time: endDate.toISOString(),
        compare_to_previous: true,
      },
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  const apiTrafficQuery = useQuery({
    ...getApiTimeseriesOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
      },
    }),
    enabled: !!project.id,
  })

  const apiRoutesQuery = useQuery({
    ...getApiRoutesOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        limit: 5,
        offset: 0,
      },
    }),
    enabled: !!project.id,
  })

  const recentErrorsQuery = useQuery({
    ...listErrorGroupsOptions({
      path: { project_id: project.id },
      query: {
        page: 1,
        page_size: 5,
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        sort_by: 'last_seen',
        sort_order: 'desc',
      },
    }),
    enabled: !!project.id,
  })

  const tracesQuery = useQuery({
    ...queryTraceSummariesOptions({
      query: {
        project_id: project.id,
        start_time: startDate.toISOString(),
        end_time: endDate.toISOString(),
        include_total: true,
        limit: 1,
        offset: 0,
      },
    }),
    enabled: !!project.id,
  })

  const apiTraffic = apiTrafficQuery.data
  const traceCount = tracesQuery.data?.total ?? 0
  const applicationErrors = errorStats?.total_errors ?? 0
  const errorGroups = errorStats?.error_groups ?? 0
  const trafficHealthData = useMemo(
    () =>
      (apiTraffic?.points ?? []).map((point) => ({
        time: format(new Date(point.timestamp), 'HH:mm'),
        p95: point.p95_latency_ms,
        p99: point.p99_latency_ms,
        errorRate: point.error_rate * 100,
      })),
    [apiTraffic?.points]
  )
  const latencySampleCount = trafficHealthData.filter(
    (point) => Number.isFinite(point.p95) && Number.isFinite(point.p99)
  ).length
  const hasLatencyTrend =
    trafficHealthData.length >= 4 &&
    latencySampleCount / trafficHealthData.length >= 0.5
  const topRoutes = apiRoutesQuery.data?.routes ?? []
  const recentErrors = recentErrorsQuery.data?.data ?? []
  const visitors = visitorsQuery.data?.count ?? 0
  const returningVisitors = returningVisitorsQuery.data?.count ?? 0
  const returningVisitorRate =
    visitors > 0 ? (returningVisitors / visitors) * 100 : 0

  return (
    <>
      <div className="@container/overview">
        <div className="grid gap-4 @5xl/overview:grid-cols-5">
          <Card className="@5xl/overview:col-span-3">
            <CardHeader className="flex flex-row items-start justify-between gap-4 pb-2">
              <div className="min-w-0">
                <h2 className="truncate text-base font-semibold">
                  Traffic health
                </h2>
                <p className="text-base text-pretty text-muted-foreground sm:text-sm">
                  {formatNumber(apiTraffic?.total_requests)} requests and{' '}
                  {formatNumber(traceCount)} traces in the last 24 hours.
                </p>
              </div>
              <Button variant="ghost" size="sm" asChild>
                <Link to={`/projects/${project.slug}/analytics/api-traffic`}>
                  Explore
                  <ArrowRight className="size-4 shrink-0" />
                </Link>
              </Button>
            </CardHeader>
            <CardContent>
              {apiTrafficQuery.isPending ? (
                <Skeleton className="h-52 w-full" />
              ) : apiTrafficQuery.isError ? (
                <SignalError message="API traffic is temporarily unavailable." />
              ) : (
                <div className="grid gap-5 @3xl/overview:grid-cols-2">
                  <div className="min-w-0">
                    <div className="flex items-baseline justify-between gap-3">
                      <h3 className="truncate text-sm font-medium">
                        Tail latency
                      </h3>
                      <p className="text-sm text-muted-foreground">p95 / p99</p>
                    </div>
                    {hasLatencyTrend ? (
                      <ThresholdLineChart
                        data={trafficHealthData}
                        xKey="time"
                        series={[
                          { dataKey: 'p95', label: 'p95', tone: 'neutral' },
                          { dataKey: 'p99', label: 'p99', tone: 'warn' },
                        ]}
                        height={190}
                        yTickFormatter={(value) => `${Math.round(value)}ms`}
                        tooltipValueFormatter={(value) =>
                          `${Math.round(value)}ms`
                        }
                        emptyMessage={
                          <CompactEmpty label="No latency samples" />
                        }
                      />
                    ) : (
                      <SparseTrendState
                        samples={latencySampleCount}
                        label="latency"
                      />
                    )}
                  </div>
                  <div className="min-w-0">
                    <div className="flex items-baseline justify-between gap-3">
                      <h3 className="truncate text-sm font-medium">
                        HTTP error rate
                      </h3>
                      <p
                        className={cn(
                          'text-sm font-medium tabular-nums',
                          (apiTraffic?.overall_error_rate ?? 0) > 0.05
                            ? 'text-amber-600 dark:text-amber-400'
                            : 'text-muted-foreground'
                        )}
                      >
                        {formatPercent(apiTraffic?.overall_error_rate)} overall
                      </p>
                    </div>
                    <ThresholdLineChart
                      data={trafficHealthData}
                      xKey="time"
                      series={{
                        dataKey: 'errorRate',
                        label: 'Error rate',
                        tone: 'poor',
                      }}
                      height={190}
                      yTickFormatter={(value) => `${value.toFixed(0)}%`}
                      tooltipValueFormatter={(value) => `${value.toFixed(1)}%`}
                      emptyMessage={<CompactEmpty label="No error samples" />}
                    />
                  </div>
                </div>
              )}
            </CardContent>
          </Card>

          <VisitorAnalyticsCard
            projectSlug={project.slug}
            capabilityPending={analyticsCapabilityQuery.isPending}
            capabilityError={analyticsCapabilityQuery.isError}
            enabled={analyticsEnabled}
            visitors={visitors}
            pageViews={pageViewsQuery.data?.count ?? 0}
            returningVisitors={returningVisitors}
            returningRate={returningVisitorRate}
            hourlyVisitors={hourlyVisitorsQuery.data ?? []}
            loading={
              visitorsQuery.isPending ||
              pageViewsQuery.isPending ||
              returningVisitorsQuery.isPending ||
              hourlyVisitorsQuery.isPending
            }
            error={
              visitorsQuery.isError ||
              pageViewsQuery.isError ||
              returningVisitorsQuery.isError ||
              hourlyVisitorsQuery.isError
            }
          />
        </div>

        <div className="mt-4 grid gap-4 @5xl/overview:grid-cols-5">
          <EnvironmentHealthCard project={project} />
        </div>

        <div className="mt-4 grid gap-4 @5xl/overview:grid-cols-5">
          <TopRoutesCard
            projectSlug={project.slug}
            routes={topRoutes}
            loading={apiRoutesQuery.isPending}
            error={apiRoutesQuery.isError}
          />
          <RecentErrorsCard
            projectSlug={project.slug}
            errors={recentErrors}
            totalErrors={applicationErrors}
            errorGroups={errorGroups}
            loading={recentErrorsQuery.isPending || isLoadingErrors}
            error={recentErrorsQuery.isError || hasErrorStatsError}
          />
        </div>
      </div>

      <div className="mt-4 sm:mt-6">
        {lastDeployment && (
          <LastDeployment
            deployment={lastDeployment}
            projectName={project.slug}
          />
        )}
      </div>

      <div className="mt-6 sm:mt-8">
        <RecentDeployments project={project} />
      </div>

      <div className="mt-4 flex justify-center sm:mt-6">
        <button
          type="button"
          onClick={() => window.dispatchEvent(new Event(PROJECT_TOUR_EVENT))}
          className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        >
          <Sparkles className="size-4 shrink-0" />
          Take a tour of your project
        </button>
      </div>
    </>
  )
}

function VisitorAnalyticsCard({
  projectSlug,
  capabilityPending,
  capabilityError,
  enabled,
  visitors,
  pageViews,
  returningVisitors,
  returningRate,
  hourlyVisitors,
  loading,
  error,
}: {
  projectSlug: string
  capabilityPending: boolean
  capabilityError: boolean
  enabled: boolean
  visitors: number
  pageViews: number
  returningVisitors: number
  returningRate: number
  hourlyVisitors: EventTimeline[]
  loading: boolean
  error: boolean
}) {
  return (
    <Card className="@5xl/overview:col-span-2">
      <CardHeader className="flex flex-row items-start justify-between gap-4 pb-2">
        <div className="min-w-0">
          <h2 className="truncate text-base font-semibold">
            Visitor analytics
          </h2>
          <p className="text-base text-pretty text-muted-foreground sm:text-sm">
            Browser activity from the last 24 hours.
          </p>
        </div>
        {enabled && (
          <Button variant="ghost" size="sm" asChild>
            <Link to={`/projects/${projectSlug}/analytics`}>
              Explore
              <ArrowRight className="size-4 shrink-0" />
            </Link>
          </Button>
        )}
      </CardHeader>
      <CardContent>
        {capabilityPending ? (
          <Skeleton className="h-52 w-full" />
        ) : capabilityError ? (
          <SignalError message="Visitor analytics is temporarily unavailable." />
        ) : !enabled ? (
          <div className="flex h-52 flex-col items-start justify-center gap-3 rounded-md border border-dashed p-5">
            <div>
              <p className="font-medium">Add browser analytics</p>
              <p className="mt-1 text-base text-pretty text-muted-foreground sm:text-sm">
                Track visitors, page views, and returning activity from this
                project.
              </p>
            </div>
            <Button variant="outline" size="sm" asChild>
              <Link to={`/projects/${projectSlug}/analytics/setup`}>
                Set up analytics
              </Link>
            </Button>
          </div>
        ) : loading ? (
          <Skeleton className="h-52 w-full" />
        ) : error ? (
          <SignalError message="Visitor analytics is temporarily unavailable." />
        ) : (
          <div className="flex flex-col gap-4">
            <VisitorSparkline
              data={hourlyVisitors.map((point) => ({
                hour: point.date,
                count: point.count,
              }))}
              height={82}
              className="w-full"
            />
            <dl className="grid grid-cols-2 gap-x-5 gap-y-3">
              <AnalyticsValue label="Visitors" value={formatNumber(visitors)} />
              <AnalyticsValue
                label="Page views"
                value={formatNumber(pageViews)}
              />
              <AnalyticsValue
                label="Returning visitors"
                value={formatNumber(returningVisitors)}
              />
              <AnalyticsValue
                label="Returning rate"
                value={`${returningRate.toFixed(0)}%`}
              />
            </dl>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function EnvironmentHealthCard({ project }: { project: ProjectResponse }) {
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })
  // Same "first environment" convention environment-navigation.ts uses when
  // no environment is explicitly selected in the URL.
  const environment = environmentsQuery.data?.[0]

  const historyQuery = useQuery({
    ...listContainerHistoryOptions({
      path: { project_id: project.id, environment_id: environment?.id ?? 0 },
    }),
    enabled: !!environment?.id,
  })

  const currentContainers = useMemo(
    () => (historyQuery.data?.containers ?? []).filter((c) => c.is_current),
    [historyQuery.data]
  )

  const historyOptionsFor = (containerId: string, metric: string) => ({
    ...containerMetricsGetHistoryOptions({
      path: {
        project_id: project.id,
        environment_id: environment?.id ?? 0,
        container_id: containerId,
      },
      query: { metric, range: '1h' },
    }),
    enabled: !!environment?.id,
    staleTime: 30_000,
    retry: false,
  })

  const cpuQueries = useQueries({
    queries: currentContainers.map((c) =>
      historyOptionsFor(c.container_id, 'container.cpu_percent')
    ),
  })
  const memQueries = useQueries({
    queries: currentContainers.map((c) =>
      historyOptionsFor(c.container_id, 'container.memory_used_bytes')
    ),
  })

  const cpuData = useMemo(
    () =>
      sumMetricSeries(cpuQueries.map((q) => q.data ?? [])).map((point) => ({
        time: format(new Date(point.time), 'HH:mm'),
        value: point.value,
      })),
    [cpuQueries]
  )
  const memData = useMemo(
    () =>
      sumMetricSeries(memQueries.map((q) => q.data ?? [])).map((point) => ({
        time: format(new Date(point.time), 'HH:mm'),
        value: point.value / (1024 * 1024),
      })),
    [memQueries]
  )

  const isLoading =
    environmentsQuery.isPending || (!!environment?.id && historyQuery.isPending)
  const notConfigured =
    currentContainers.length > 0 &&
    cpuQueries.length === currentContainers.length &&
    cpuQueries.every(
      (q) =>
        q.isError &&
        metricsErrorDetail(q.error).toLowerCase().includes('not enabled')
    )
  const hasCpuTrend = cpuData.length >= 4
  const hasMemTrend = memData.length >= 4

  return (
    <Card className="@5xl/overview:col-span-5">
      <CardHeader className="flex flex-row items-start justify-between gap-4 pb-2">
        <div className="min-w-0">
          <h2 className="truncate text-base font-semibold">
            {environment ? `${environment.name} environment` : 'Environment'}{' '}
            health
          </h2>
          <p className="text-base text-pretty text-muted-foreground sm:text-sm">
            {currentContainers.length === 0
              ? 'CPU and memory usage over the last hour.'
              : `${currentContainers.length} running container${currentContainers.length === 1 ? '' : 's'} over the last hour.`}
          </p>
        </div>
        <Button variant="ghost" size="sm" asChild>
          <Link to={`/projects/${project.slug}/environments?view=metrics`}>
            Explore
            <ArrowRight className="size-4 shrink-0" />
          </Link>
        </Button>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <Skeleton className="h-52 w-full" />
        ) : !environment ? (
          <SignalError message="No environments found for this project." />
        ) : notConfigured ? (
          <div className="flex h-52 flex-col items-start justify-center gap-3 rounded-md border border-dashed p-5">
            <div>
              <p className="font-medium">Track CPU and memory over time</p>
              <p className="mt-1 text-base text-pretty text-muted-foreground sm:text-sm">
                Once enabled, this card shows CPU and memory usage for{' '}
                {environment.name} sampled over the last hour.
              </p>
            </div>
            <Button variant="outline" size="sm" asChild>
              <Link to="/settings/metrics-monitoring">
                Enable metrics collection
              </Link>
            </Button>
          </div>
        ) : currentContainers.length === 0 ? (
          <CompactEmpty label="No running containers in this environment" />
        ) : (
          <div className="grid gap-5 @3xl/overview:grid-cols-2">
            <div className="min-w-0">
              <h3 className="truncate text-sm font-medium">CPU usage</h3>
              {hasCpuTrend ? (
                <ThresholdLineChart
                  data={cpuData}
                  xKey="time"
                  series={{ dataKey: 'value', label: 'CPU', tone: 'neutral' }}
                  height={150}
                  yTickFormatter={(value) => `${Math.round(value)}%`}
                  tooltipValueFormatter={(value) => `${value.toFixed(1)}%`}
                  emptyMessage={<CompactEmpty label="No CPU samples" />}
                />
              ) : (
                <SparseTrendState samples={cpuData.length} label="CPU" />
              )}
            </div>
            <div className="min-w-0">
              <h3 className="truncate text-sm font-medium">Memory usage</h3>
              {hasMemTrend ? (
                <ThresholdLineChart
                  data={memData}
                  xKey="time"
                  series={{
                    dataKey: 'value',
                    label: 'Memory',
                    tone: 'neutral',
                  }}
                  height={150}
                  yTickFormatter={(value) => `${Math.round(value)} MB`}
                  tooltipValueFormatter={(value) => `${value.toFixed(0)} MB`}
                  emptyMessage={<CompactEmpty label="No memory samples" />}
                />
              ) : (
                <SparseTrendState samples={memData.length} label="memory" />
              )}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function AnalyticsValue({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="truncate text-sm font-medium">{label}</dt>
      <dd className="mt-0.5 text-lg text-muted-foreground tabular-nums">
        {value}
      </dd>
    </div>
  )
}

function TopRoutesCard({
  projectSlug,
  routes,
  loading,
  error,
}: {
  projectSlug: string
  routes: ApiRouteEntry[]
  loading: boolean
  error: boolean
}) {
  return (
    <Card className="@5xl/overview:col-span-3">
      <CardHeader className="flex flex-row items-start justify-between gap-4 pb-2">
        <div className="min-w-0">
          <h2 className="truncate text-base font-semibold">
            Most invoked paths
          </h2>
          <p className="text-base text-pretty text-muted-foreground sm:text-sm">
            Top five routes by request volume in the last 24 hours.
          </p>
        </div>
        <Button variant="ghost" size="sm" asChild>
          <Link to={`/projects/${projectSlug}/analytics/api-traffic`}>
            All paths
            <ArrowRight className="size-4 shrink-0" />
          </Link>
        </Button>
      </CardHeader>
      <CardContent className="p-0">
        {loading ? (
          <div className="space-y-3 px-5 pb-5">
            {Array.from({ length: 5 }).map((_, index) => (
              <Skeleton key={index} className="h-10 w-full" />
            ))}
          </div>
        ) : error ? (
          <div className="px-5 pb-5">
            <SignalError message="Route traffic is temporarily unavailable." />
          </div>
        ) : routes.length === 0 ? (
          <CompactListEmpty
            title="No paths recorded"
            detail="Routes appear here as soon as the project receives traffic."
          />
        ) : (
          <div>
            <div className="hidden grid-cols-[minmax(0,1fr)_5rem_5rem_5rem] gap-3 border-b border-border/60 px-5 py-2 text-sm font-medium text-muted-foreground sm:grid">
              <p>Path</p>
              <p className="text-right">Requests</p>
              <p className="text-right">Latency</p>
              <p className="text-right">Errors</p>
            </div>
            <div className="divide-y divide-border/60">
              {routes.map((route) => (
                <Link
                  key={`${route.method}:${route.path}`}
                  to={`/projects/${projectSlug}/analytics/api-traffic`}
                  className="grid min-w-0 gap-2 px-5 py-3 hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring sm:grid-cols-[minmax(0,1fr)_5rem_5rem_5rem] sm:items-center sm:gap-3"
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <Badge variant="outline" className="shrink-0">
                      {route.method}
                    </Badge>
                    <p
                      className="truncate font-mono text-sm"
                      title={route.path}
                    >
                      {route.path}
                    </p>
                  </div>
                  <div className="grid grid-cols-3 gap-3 sm:contents">
                    <RouteMetric
                      mobileLabel="Requests"
                      value={formatNumber(route.request_count)}
                    />
                    <RouteMetric
                      mobileLabel="Latency"
                      value={formatMilliseconds(route.avg_latency_ms)}
                    />
                    <RouteMetric
                      mobileLabel="Errors"
                      value={formatPercent(route.error_rate)}
                      warning={route.error_rate > 0.05}
                    />
                  </div>
                </Link>
              ))}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function RouteMetric({
  mobileLabel,
  value,
  warning = false,
}: {
  mobileLabel: string
  value: string
  warning?: boolean
}) {
  return (
    <div className="min-w-0 text-right">
      <p className="truncate text-sm text-muted-foreground sm:hidden">
        {mobileLabel}
      </p>
      <p
        className={cn(
          'truncate text-sm font-medium tabular-nums',
          warning && 'text-amber-600 dark:text-amber-400'
        )}
      >
        {value}
      </p>
    </div>
  )
}

function RecentErrorsCard({
  projectSlug,
  errors,
  totalErrors,
  errorGroups,
  loading,
  error,
}: {
  projectSlug: string
  errors: ErrorGroupResponse[]
  totalErrors: number
  errorGroups: number
  loading: boolean
  error: boolean
}) {
  return (
    <Card className="@5xl/overview:col-span-2">
      <CardHeader className="flex flex-row items-start justify-between gap-4 pb-2">
        <div className="min-w-0">
          <h2 className="truncate text-base font-semibold">Latest errors</h2>
          <p className="text-base text-pretty text-muted-foreground sm:text-sm">
            {formatNumber(totalErrors)} events across{' '}
            {formatNumber(errorGroups)}
            {' error groups.'}
          </p>
        </div>
        <Button variant="ghost" size="sm" asChild>
          <Link to={`/projects/${projectSlug}/errors`}>
            All errors
            <ArrowRight className="size-4 shrink-0" />
          </Link>
        </Button>
      </CardHeader>
      <CardContent className="p-0">
        {loading ? (
          <div className="space-y-3 px-5 pb-5">
            {Array.from({ length: 5 }).map((_, index) => (
              <Skeleton key={index} className="h-10 w-full" />
            ))}
          </div>
        ) : error ? (
          <div className="px-5 pb-5">
            <SignalError message="Error tracking is temporarily unavailable." />
          </div>
        ) : errors.length === 0 ? (
          <CompactListEmpty
            title="No recent errors"
            detail="Caught application errors will appear here with their latest occurrence."
          />
        ) : (
          <div className="divide-y divide-border/60">
            {errors.map((item) => (
              <Link
                key={item.id}
                to={`/projects/${projectSlug}/errors/${item.id}`}
                className="flex min-w-0 items-start justify-between gap-3 px-5 py-3 hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
              >
                <div className="min-w-0">
                  <p className="truncate font-medium" title={item.title}>
                    {item.title}
                  </p>
                  <p className="mt-0.5 truncate text-sm text-muted-foreground">
                    {item.error_type} · {formatNumber(item.total_count)} events
                  </p>
                </div>
                <p className="shrink-0 text-sm text-muted-foreground">
                  <TimeAgo date={item.last_seen} />
                </p>
              </Link>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function CompactListEmpty({
  title,
  detail,
}: {
  title: string
  detail: string
}) {
  return (
    <div className="flex min-h-44 flex-col items-center justify-center gap-1 px-5 pb-5 text-center">
      <p className="font-medium">{title}</p>
      <p className="max-w-sm text-base text-pretty text-muted-foreground sm:text-sm">
        {detail}
      </p>
    </div>
  )
}

function CompactEmpty({ label }: { label: string }) {
  return <p className="text-sm text-muted-foreground">{label}</p>
}

function SparseTrendState({
  samples,
  label,
}: {
  samples: number
  label: string
}) {
  return (
    <div className="flex h-[190px] flex-col items-center justify-center rounded-md border border-dashed px-5 text-center">
      <p className="text-sm font-medium">Not enough data for a trend</p>
      <p className="mt-1 max-w-xs text-sm text-muted-foreground">
        {samples === 0
          ? `No ${label} samples were recorded in this window.`
          : `${samples} sampled ${samples === 1 ? 'bucket is' : 'buckets are'} not enough to show a reliable chart.`}
      </p>
    </div>
  )
}

function SignalError({ message }: { message: string }) {
  return (
    <div className="flex h-56 items-center justify-center rounded-md border border-dashed px-5 text-center">
      <p className="text-base text-muted-foreground sm:text-sm">{message}</p>
    </div>
  )
}

/** Sums same-timestamp points across containers into one aggregate series. */
function sumMetricSeries(
  seriesList: { time: string; value: number }[][]
): { time: string; value: number }[] {
  const sums = new Map<string, number>()
  for (const points of seriesList) {
    for (const point of points) {
      sums.set(point.time, (sums.get(point.time) ?? 0) + point.value)
    }
  }
  return Array.from(sums.entries())
    .map(([time, value]) => ({ time, value }))
    .sort((a, b) => new Date(a.time).getTime() - new Date(b.time).getTime())
}

/** `@hey-api/client-fetch` throws the parsed RFC 7807 Problem body
 *  ({ detail, title, status }) on a failed request. */
function metricsErrorDetail(err: unknown): string {
  if (err == null) return ''
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  const problem = err as { detail?: string; title?: string }
  return problem.detail ?? problem.title ?? ''
}

function formatNumber(value: number | null | undefined): string {
  return (value ?? 0).toLocaleString()
}

function formatPercent(value: number | null | undefined): string {
  return `${((value ?? 0) * 100).toFixed(1)}%`
}

function formatMilliseconds(value: number | null | undefined): string {
  if (value == null) return '—'
  if (value < 1) return '<1ms'
  return `${Math.round(value).toLocaleString()}ms`
}
