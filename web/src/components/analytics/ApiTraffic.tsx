import {
  getApiCallersOptions,
  getApiTrafficProxyLogAccessOptions,
  getApiRoutesOptions,
  getApiSummaryOptions,
  getApiTimeseriesOptions,
  getProjectBySlugQueryKey,
  getProjectDeploymentsOptions,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { aggregateApiTraffic, getApiSummary } from '@/api/client'
import type {
  ProjectResponse,
  TrafficAggregationRequest,
  TrafficAggregationResponse,
  TrafficAggregationRow,
  TrafficFilter,
} from '@/api/client/types.gen'
import {
  ThresholdLineChart,
  type ThresholdMarker,
} from '@/components/charts/threshold-line-chart'
import { AnalyticsFilters } from '@/components/project/ProjectAnalytics'
import { Badge } from '@/components/ui/badge'
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
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Link } from 'react-router'
import {
  getDateRangeFromFilter,
  QUICK_FILTERS,
  type AnalyticsDateFilter,
  type QuickFilter,
} from '@/hooks/useAnalyticsDateRange'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  ChevronLeft,
  ChevronRight,
  Loader2,
  Server,
  Sparkles,
} from 'lucide-react'
import * as React from 'react'
import { useSearchParams } from 'react-router'
import { useStructuredAi } from '@/hooks/useStructuredAi'
import { toast } from 'sonner'
import {
  apiTrafficSummaryCacheKey,
  canStartApiTrafficSummary,
  shouldRequestApiTrafficSummary,
} from '@/lib/ai-summary'
import {
  nextTrafficSort,
  trafficPageCount,
  type TrafficSort,
} from '@/lib/api-traffic-sort'
import { apiTrafficProxyLogsUrl } from '@/lib/api-traffic-navigation'

interface ApiTrafficTabProps {
  project: ProjectResponse
}

interface ApiTrafficAiSummary {
  headline?: string
  findings?: string[]
  anomalies?: string[]
  recommendation?: string | null
}

type TrafficMetric =
  | 'requests'
  | 'error_rate'
  | 'latency_avg'
  | 'latency_min'
  | 'latency_max'
  | 'latency_p95'
  | 'last_seen'

interface TrafficDetail {
  kind: 'ip' | 'path'
  value: string
  method?: string
}

function trafficDimension(row: TrafficAggregationRow, name: string): string {
  return row.dimensions.find((item) => item.dimension === name)?.value ?? '—'
}

async function queryTraffic(
  projectId: number,
  body: TrafficAggregationRequest
): Promise<TrafficAggregationResponse> {
  const { data, error } = await aggregateApiTraffic({
    path: { project_id: projectId },
    body,
  })
  if (error || !data) throw new Error(JSON.stringify(error ?? 'No response'))
  return data
}

const API_TRAFFIC_SUMMARY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['headline', 'findings', 'anomalies', 'recommendation'],
  properties: {
    headline: { type: 'string', description: 'One concise sentence.' },
    findings: {
      type: 'array',
      minItems: 2,
      maxItems: 4,
      items: { type: 'string' },
    },
    anomalies: {
      type: 'array',
      maxItems: 3,
      items: { type: 'string' },
    },
    recommendation: { type: ['string', 'null'] },
  },
} as const

const API_TRAFFIC_SUMMARY_CACHE_TTL_SECONDS = 15 * 60

function formatNumber(n: number): string {
  return n.toLocaleString('en-US')
}

function formatMs(ms: number | null | undefined): string {
  return ms == null ? '—' : `${ms.toFixed(0)}ms`
}

function formatPercent(rate: number): string {
  return `${(rate * 100).toFixed(1)}%`
}

function formatBucketLabel(bucket: string): string {
  const d = new Date(bucket)
  if (Number.isNaN(d.getTime())) return bucket
  return d.toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

export function ApiTrafficTab({ project }: ApiTrafficTabProps) {
  const [searchParams, setSearchParams] = useSearchParams()

  const [dateFilter, setDateFilter] = React.useState<AnalyticsDateFilter>(
    () => {
      const filter = searchParams.get('filter') as QuickFilter | null
      const from = searchParams.get('from')
      const to = searchParams.get('to')

      if (filter === 'custom' && from && to) {
        return {
          quickFilter: 'custom',
          dateRange: { from: new Date(from), to: new Date(to) },
        }
      }
      if (filter && QUICK_FILTERS.some((f) => f.value === filter)) {
        return { quickFilter: filter, dateRange: undefined }
      }
      return { quickFilter: '24hours', dateRange: undefined }
    }
  )
  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const [summaryRequested, setSummaryRequested] = React.useState(false)
  const [summaryRefreshNonce, setSummaryRefreshNonce] = React.useState(0)
  const forceCliSummaryRefresh = React.useRef(false)
  const [routePage, setRoutePage] = React.useState(0)
  const [callerPage, setCallerPage] = React.useState(0)
  const [routeSort, setRouteSort] = React.useState<TrafficSort<TrafficMetric>>({
    metric: 'requests',
    direction: 'desc',
  })
  const [callerSort, setCallerSort] = React.useState<
    TrafficSort<TrafficMetric>
  >({ metric: 'requests', direction: 'desc' })
  const [detail, setDetail] = React.useState<TrafficDetail | null>(null)
  const [detailPage, setDetailPage] = React.useState(0)
  const [detailSort, setDetailSort] = React.useState<
    TrafficSort<TrafficMetric>
  >({ metric: 'requests', direction: 'desc' })
  const queryClient = useQueryClient()

  // getDateRangeFromFilter computes `new Date()` internally, so calling it
  // unmemoized would produce a new (millisecond-different) ISO string on
  // every render for the relative quick-filters — a different React Query
  // key each time, which refetches, which re-renders, which recomputes a new
  // "now" again: an infinite fetch loop. Memoizing on `dateFilter` freezes
  // the range for as long as the user's selection is unchanged.
  const { startDate, endDate } = React.useMemo(
    () => getDateRangeFromFilter(dateFilter),
    [dateFilter]
  )
  const startIso = startDate?.toISOString()
  const endIso = endDate?.toISOString()

  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
      setRoutePage(0)
      setCallerPage(0)
      setDetailPage(0)
      setSummaryRequested(false)
      const params = new URLSearchParams(searchParams)
      params.set('filter', next.quickFilter)
      if (
        next.quickFilter === 'custom' &&
        next.dateRange?.from &&
        next.dateRange?.to
      ) {
        params.set('from', next.dateRange.from.toISOString())
        params.set('to', next.dateRange.to.toISOString())
      } else {
        params.delete('from')
        params.delete('to')
      }
      setSearchParams(params, { replace: true })
    },
    [searchParams, setSearchParams]
  )

  const handleRefresh = React.useCallback(() => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as { _id?: string } | string
        const id = typeof key === 'string' ? key : key?._id
        return (
          (typeof key === 'string' && key === 'api-traffic-query') ||
          (!!id && /getApi(Timeseries|Routes|Callers)/.test(id))
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  const handleSummaryRefresh = React.useCallback(() => {
    forceCliSummaryRefresh.current = true
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as { _id?: string } | string
        const id = typeof key === 'string' ? key : key?._id
        return id === 'getApiSummary'
      },
    })
    setSummaryRequested(true)
    setSummaryRefreshNonce((value) => value + 1)
  }, [queryClient])

  const enabled = !!startIso && !!endIso
  const pageSize = 20
  const proxyLogAccessQuery = useQuery({
    ...getApiTrafficProxyLogAccessOptions({
      path: { project_id: project.id },
    }),
    staleTime: 5 * 60 * 1000,
  })

  const timeseriesQuery = useQuery({
    ...getApiTimeseriesOptions({
      path: { project_id: project.id },
      query: {
        start_date: startIso ?? '',
        end_date: endIso ?? '',
        environment_id: selectedEnvironment,
      },
    }),
    enabled,
  })

  const routesQuery = useQuery({
    queryKey: [
      'api-traffic-query',
      project.id,
      startIso,
      endIso,
      selectedEnvironment,
      'routes',
      routePage,
      routeSort.metric,
      routeSort.direction,
    ],
    queryFn: () =>
      queryTraffic(project.id, {
        start_time: startIso ?? '',
        end_time: endIso ?? '',
        environment_id: selectedEnvironment,
        dimensions: ['method', 'path'],
        metrics: [
          'requests',
          'errors',
          'error_rate',
          'latency_avg',
          'latency_min',
          'latency_max',
          'latency_p95',
          'unique_ips',
          'bot_requests',
          'robots_txt_requests',
          'last_seen',
        ],
        filters: [],
        order_by: [
          {
            field: { kind: 'metric', field: routeSort.metric },
            direction: routeSort.direction,
          },
        ],
        include_synthetic: false,
        page: routePage + 1,
        page_size: pageSize,
      }),
    enabled,
  })

  const callersQuery = useQuery({
    queryKey: [
      'api-traffic-query',
      project.id,
      startIso,
      endIso,
      selectedEnvironment,
      'callers',
      callerPage,
      callerSort.metric,
      callerSort.direction,
    ],
    queryFn: () =>
      queryTraffic(project.id, {
        start_time: startIso ?? '',
        end_time: endIso ?? '',
        environment_id: selectedEnvironment,
        dimensions: ['client_ip'],
        metrics: [
          'requests',
          'errors',
          'error_rate',
          'latency_avg',
          'latency_min',
          'latency_max',
          'latency_p95',
          'unique_paths',
          'bot_requests',
          'robots_txt_requests',
          'last_seen',
        ],
        filters: [],
        order_by: [
          {
            field: { kind: 'metric', field: callerSort.metric },
            direction: callerSort.direction,
          },
        ],
        include_synthetic: false,
        page: callerPage + 1,
        page_size: pageSize,
      }),
    enabled,
  })

  const detailQuery = useQuery({
    queryKey: [
      'api-traffic-query',
      project.id,
      startIso,
      endIso,
      selectedEnvironment,
      'detail',
      detail,
      detailPage,
      detailSort.metric,
      detailSort.direction,
    ],
    queryFn: () => {
      if (!detail) throw new Error('No traffic detail selected')
      const filters: TrafficFilter[] =
        detail.kind === 'ip'
          ? [{ dimension: 'client_ip', operator: 'eq', values: [detail.value] }]
          : [{ dimension: 'path', operator: 'eq', values: [detail.value] }]
      if (detail.kind === 'path' && detail.method) {
        filters.push({
          dimension: 'method',
          operator: 'eq',
          values: [detail.method],
        })
      }
      return queryTraffic(project.id, {
        start_time: startIso ?? '',
        end_time: endIso ?? '',
        environment_id: selectedEnvironment,
        dimensions: detail.kind === 'ip' ? ['method', 'path'] : ['client_ip'],
        metrics: [
          'requests',
          'errors',
          'error_rate',
          'latency_avg',
          'latency_min',
          'latency_max',
          'latency_p95',
          'bot_requests',
          'robots_txt_requests',
          'last_seen',
        ],
        filters,
        order_by: [
          {
            field: { kind: 'metric', field: detailSort.metric },
            direction: detailSort.direction,
          },
        ],
        include_synthetic: false,
        page: detailPage + 1,
        page_size: pageSize,
      })
    },
    enabled: enabled && detail !== null,
  })

  // Stable first-page context for the summary. These legacy response shapes
  // differ from the visible generic aggregates, so defer both scans until the
  // user explicitly asks to spend AI credits.
  const summaryRoutesQuery = useQuery({
    ...getApiRoutesOptions({
      path: { project_id: project.id },
      query: {
        start_date: startIso ?? '',
        end_date: endIso ?? '',
        environment_id: selectedEnvironment,
        limit: pageSize,
        offset: 0,
      },
    }),
    enabled: enabled && summaryRequested,
  })
  const summaryCallersQuery = useQuery({
    ...getApiCallersOptions({
      path: { project_id: project.id },
      query: {
        start_date: startIso ?? '',
        end_date: endIso ?? '',
        environment_id: selectedEnvironment,
        limit: pageSize,
        offset: 0,
      },
    }),
    enabled: enabled && summaryRequested,
  })

  // Wait for the on-demand context queries before requesting the summary. The
  // backend keeps a small, short-lived cache keyed by this exact window.
  const summaryDataReady =
    enabled &&
    summaryRequested &&
    !timeseriesQuery.isPending &&
    !summaryRoutesQuery.isPending &&
    !summaryCallersQuery.isPending
  const summaryPrompt = React.useMemo(() => {
    const timeseries = timeseriesQuery.data
    const routes = summaryRoutesQuery.data
    const callers = summaryCallersQuery.data
    if (!summaryDataReady || !timeseries || !routes || !callers) return null
    const highErrorBuckets = timeseries.points
      .filter((point) => point.request_count > 0 && point.error_rate > 0.1)
      .slice(0, 5)
      .map((point) => ({
        timestamp: point.timestamp,
        requests: point.request_count,
        errors: point.error_count,
        error_rate: point.error_rate,
        p95_latency_ms: point.p95_latency_ms,
      }))
    return [
      'You are a concise API traffic analyst. Return only JSON matching the schema. Focus on anomalies, high error rates, and latency outliers. Never include raw IP addresses.',
      JSON.stringify({
        period: { start: startIso, end: endIso },
        overall: {
          requests: timeseries.total_requests,
          errors: timeseries.total_errors,
          error_rate: timeseries.overall_error_rate,
          average_latency_ms: timeseries.overall_avg_latency_ms,
          bucket_interval: timeseries.bucket_interval,
        },
        high_error_buckets: highErrorBuckets,
        // Raw methods and paths are intentionally excluded: request paths are
        // attacker-controlled and may contain identifiers, secrets, or prompt
        // injection text. Numeric ranks preserve the traffic distribution.
        routes: routes.routes.slice(0, 10).map((route, index) => ({
          rank: index + 1,
          requests: route.request_count,
          average_latency_ms: route.avg_latency_ms,
          error_rate: route.error_rate,
        })),
        callers: callers.callers.slice(0, 10).map((caller, index) => ({
          rank: index + 1,
          requests: caller.request_count,
          error_rate: caller.error_rate,
        })),
        distinct_routes: routes.total_routes,
        distinct_callers: callers.total_callers,
      }),
    ].join('\n\n')
  }, [
    summaryCallersQuery.data,
    endIso,
    summaryRoutesQuery.data,
    startIso,
    summaryDataReady,
    timeseriesQuery.data,
  ])
  const summaryCacheKey = React.useMemo(() => {
    return apiTrafficSummaryCacheKey({
      environmentId: selectedEnvironment,
      filter: dateFilter.quickFilter,
      startIso,
      endIso,
    })
  }, [dateFilter.quickFilter, endIso, selectedEnvironment, startIso])
  const summaryRequestEnabled = shouldRequestApiTrafficSummary({
    requested: summaryRequested,
    dataReady: summaryDataReady,
    consentEnabled: project.ai_api_traffic_summary_enabled === true,
  })
  const structuredSummary = useStructuredAi<ApiTrafficAiSummary>({
    projectId: project.id,
    purpose: 'api_traffic.summary',
    schema: API_TRAFFIC_SUMMARY_SCHEMA,
    enabled: summaryRequestEnabled,
    prompt: summaryPrompt,
    cacheKey: summaryCacheKey,
    cacheTtlSeconds: API_TRAFFIC_SUMMARY_CACHE_TTL_SECONDS,
    refreshNonce: summaryRefreshNonce,
  })
  // Generic browser-authored prompts intentionally use tool-less gateway API
  // keys. If none exists, fall back to the server-authored summary endpoint,
  // which can safely use the active Claude Code, Codex, or OpenCode adapter.
  const cliSummaryRequest = {
    path: { project_id: project.id },
    query: {
      start_date: startIso ?? '',
      end_date: endIso ?? '',
      environment_id: selectedEnvironment,
    },
  }
  const cliSummaryQuery = useQuery({
    ...getApiSummaryOptions(cliSummaryRequest),
    queryFn: async ({ signal }) => {
      const refresh = forceCliSummaryRefresh.current
      forceCliSummaryRefresh.current = false
      const { data } = await getApiSummary({
        ...cliSummaryRequest,
        query: { ...cliSummaryRequest.query, refresh },
        signal,
        throwOnError: true,
      })
      return data
    },
    enabled: summaryRequestEnabled && structuredSummary.failureStatus === 409,
  })
  const summaryData =
    structuredSummary.value ?? cliSummaryQuery.data?.summary ?? undefined
  const summaryError =
    cliSummaryQuery.data?.unavailable_reason ??
    (structuredSummary.failureStatus === 409
      ? undefined
      : structuredSummary.error)

  const points = React.useMemo(
    () => timeseriesQuery.data?.points ?? [],
    [timeseriesQuery.data?.points]
  )

  // Deploy markers — same snap-to-nearest-bucket pattern as MetricsExplorer's
  // OTEL charts, adapted for this endpoint's ISO-string buckets (proxy_logs
  // timestamps serialize with a Z suffix, unlike the epoch-number fields on
  // DeploymentResponse).
  const deploysQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: {
        per_page: 50,
        ...(selectedEnvironment != null
          ? { environment_id: selectedEnvironment }
          : {}),
      },
    }),
    enabled: enabled && points.length > 0,
  })

  const deployMarkers = React.useMemo<ThresholdMarker[]>(() => {
    const deploys = deploysQuery.data?.deployments ?? []
    if (points.length === 0 || deploys.length === 0 || !startDate || !endDate)
      return []
    const fromMs = startDate.getTime()
    const toMs = endDate.getTime()
    const toMs10 = (n: number) => (n < 1e12 ? n * 1000 : n)
    const bucketMs = points.map((p) => new Date(p.timestamp).getTime())
    const markers: ThresholdMarker[] = []
    for (const d of deploys) {
      const at = d.finished_at ?? d.started_at ?? d.created_at
      const ts = toMs10(at)
      if (ts < fromMs || ts > toMs) continue
      let best = 0
      let bestDiff = Infinity
      for (let i = 0; i < bucketMs.length; i++) {
        const diff = Math.abs(bucketMs[i] - ts)
        if (diff < bestDiff) {
          bestDiff = diff
          best = i
        }
      }
      markers.push({
        x: formatBucketLabel(points[best].timestamp),
        label: d.commit_hash ? d.commit_hash.slice(0, 7) : 'deploy',
        title: d.commit_message ?? undefined,
      })
    }
    return markers
  }, [deploysQuery.data, points, startDate, endDate])

  const chartData = React.useMemo(
    () =>
      points.map((p) => ({
        bucket: formatBucketLabel(p.timestamp),
        p95: p.p95_latency_ms ?? undefined,
        p99: p.p99_latency_ms ?? undefined,
        requests: p.request_count,
        errors: p.error_count,
      })),
    [points]
  )

  return (
    <div className="space-y-6">
      <AnalyticsFilters
        project={project}
        activeFilter={dateFilter.quickFilter}
        dateRange={dateFilter.dateRange}
        selectedEnvironment={selectedEnvironment}
        onFilterChange={(filter) =>
          updateDateFilter({ ...dateFilter, quickFilter: filter })
        }
        onDateRangeChange={(range) =>
          updateDateFilter({
            quickFilter: range ? 'custom' : dateFilter.quickFilter,
            dateRange: range,
          })
        }
        onEnvironmentChange={(environmentId) => {
          setSelectedEnvironment(environmentId)
          setRoutePage(0)
          setCallerPage(0)
          setDetailPage(0)
          setSummaryRequested(false)
        }}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      <ApiTrafficSummaryCard
        project={project}
        data={summaryData}
        error={summaryError}
        cached={
          structuredSummary.failureStatus === 409
            ? Boolean(cliSummaryQuery.data?.cached)
            : structuredSummary.cached
        }
        enabled={project.ai_api_traffic_summary_enabled === true}
        requested={summaryRequested}
        canGenerate={canStartApiTrafficSummary({
          analyticsEnabled: enabled,
          timeseriesPending: timeseriesQuery.isPending,
          timeseriesError: timeseriesQuery.isError,
        })}
        isPending={
          (summaryRequested &&
            (timeseriesQuery.isPending ||
              summaryRoutesQuery.isPending ||
              summaryCallersQuery.isPending)) ||
          structuredSummary.isPending ||
          (structuredSummary.failureStatus === 409 && cliSummaryQuery.isPending)
        }
        onGenerate={() => setSummaryRequested(true)}
        onRefresh={handleSummaryRefresh}
      />

      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <StatTile
          label="Requests"
          value={formatNumber(timeseriesQuery.data?.total_requests ?? 0)}
          loading={timeseriesQuery.isPending}
          error={timeseriesQuery.isError}
        />
        <StatTile
          label="Errors"
          value={formatNumber(timeseriesQuery.data?.total_errors ?? 0)}
          loading={timeseriesQuery.isPending}
          error={timeseriesQuery.isError}
        />
        <StatTile
          label="Error rate"
          value={formatPercent(timeseriesQuery.data?.overall_error_rate ?? 0)}
          loading={timeseriesQuery.isPending}
          error={timeseriesQuery.isError}
        />
        <StatTile
          label="Avg latency"
          value={formatMs(timeseriesQuery.data?.overall_avg_latency_ms)}
          loading={timeseriesQuery.isPending}
          error={timeseriesQuery.isError}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Latency (p95 / p99)</CardTitle>
          <CardDescription>
            Deploy markers show where a release may have shifted latency.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {timeseriesQuery.isPending ? (
            <Skeleton className="h-[300px] w-full" />
          ) : timeseriesQuery.isError ? (
            <QueryErrorState label="latency data" />
          ) : (
            <ThresholdLineChart
              data={chartData}
              xKey="bucket"
              series={[
                { dataKey: 'p95', label: 'p95', tone: 'neutral' },
                { dataKey: 'p99', label: 'p99', tone: 'warn' },
              ]}
              markers={deployMarkers}
              tooltipValueFormatter={(v) => `${v.toFixed(0)}ms`}
            />
          )}
        </CardContent>
      </Card>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Top routes</CardTitle>
            <CardDescription>
              Grouped by method + raw path — not yet template-normalized, so
              routes with dynamic IDs may appear as separate rows. Temps monitor
              checks are excluded. Select a route to inspect its callers.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {routesQuery.isPending ? (
              <Skeleton className="h-[240px] w-full" />
            ) : routesQuery.isError ? (
              <QueryErrorState label="route data" />
            ) : (routesQuery.data?.rows.length ?? 0) === 0 ? (
              <p className="text-sm text-muted-foreground">
                No API traffic for this period.
              </p>
            ) : (
              <>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Route</TableHead>
                      <SortableTrafficHead
                        label="Requests"
                        metric="requests"
                        active={routeSort}
                        onSort={(metric) => {
                          setRouteSort((current) =>
                            nextTrafficSort(current, metric)
                          )
                          setRoutePage(0)
                        }}
                      />
                      <SortableTrafficHead
                        className="hidden sm:table-cell"
                        label="Avg"
                        metric="latency_avg"
                        active={routeSort}
                        onSort={(metric) => {
                          setRouteSort((current) =>
                            nextTrafficSort(current, metric)
                          )
                          setRoutePage(0)
                        }}
                      />
                      <SortableTrafficHead
                        label="Err %"
                        metric="error_rate"
                        active={routeSort}
                        onSort={(metric) => {
                          setRouteSort((current) =>
                            nextTrafficSort(current, metric)
                          )
                          setRoutePage(0)
                        }}
                      />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {routesQuery.data?.rows.map((r, i) => {
                      const method = trafficDimension(r, 'method')
                      const path = trafficDimension(r, 'path')
                      return (
                        <TableRow
                          key={`${method}-${path}-${i}`}
                          className="cursor-pointer"
                          onClick={() => {
                            setDetailPage(0)
                            setDetailSort({
                              metric: 'requests',
                              direction: 'desc',
                            })
                            setDetail({ kind: 'path', value: path, method })
                          }}
                        >
                          <TableCell className="max-w-[220px]">
                            <div className="flex items-center gap-2">
                              <Badge variant="outline" className="shrink-0">
                                {method}
                              </Badge>
                              <span className="truncate font-mono text-xs">
                                {path}
                              </span>
                              <CrawlerSignals row={r} />
                            </div>
                          </TableCell>
                          <TableCell className="text-right">
                            {formatNumber(r.metrics.requests ?? 0)}
                          </TableCell>
                          <TableCell className="text-right hidden sm:table-cell">
                            {formatMs(r.metrics.latency_avg_ms)}
                          </TableCell>
                          <TableCell className="text-right">
                            {formatPercent(r.metrics.error_rate ?? 0)}
                          </TableCell>
                        </TableRow>
                      )
                    })}
                  </TableBody>
                </Table>
                <TrafficPagination
                  page={routePage}
                  pageSize={pageSize}
                  total={routesQuery.data?.total_groups ?? 0}
                  onPageChange={setRoutePage}
                />
              </>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Top callers</CardTitle>
            <CardDescription>
              Client IPs in this window. Temps monitor checks are excluded.
              Select an IP to inspect every route it called.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {callersQuery.isPending ? (
              <Skeleton className="h-[240px] w-full" />
            ) : callersQuery.isError ? (
              <QueryErrorState label="caller data" />
            ) : (callersQuery.data?.rows.length ?? 0) === 0 ? (
              <p className="text-sm text-muted-foreground">
                No API traffic for this period.
              </p>
            ) : (
              <>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Client IP</TableHead>
                      <SortableTrafficHead
                        label="Requests"
                        metric="requests"
                        active={callerSort}
                        onSort={(metric) => {
                          setCallerSort((current) =>
                            nextTrafficSort(current, metric)
                          )
                          setCallerPage(0)
                        }}
                      />
                      <SortableTrafficHead
                        label="Err %"
                        metric="error_rate"
                        active={callerSort}
                        onSort={(metric) => {
                          setCallerSort((current) =>
                            nextTrafficSort(current, metric)
                          )
                          setCallerPage(0)
                        }}
                      />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {callersQuery.data?.rows.map((c) => {
                      const ip = trafficDimension(c, 'client_ip')
                      return (
                        <TableRow
                          key={ip}
                          className="cursor-pointer"
                          onClick={() => {
                            setDetailPage(0)
                            setDetailSort({
                              metric: 'requests',
                              direction: 'desc',
                            })
                            setDetail({ kind: 'ip', value: ip })
                          }}
                        >
                          <TableCell>
                            <div className="flex items-center gap-2">
                              <span className="font-mono text-xs">{ip}</span>
                              <CrawlerSignals row={c} />
                            </div>
                          </TableCell>
                          <TableCell className="text-right">
                            {formatNumber(c.metrics.requests ?? 0)}
                          </TableCell>
                          <TableCell className="text-right">
                            {formatPercent(c.metrics.error_rate ?? 0)}
                          </TableCell>
                        </TableRow>
                      )
                    })}
                  </TableBody>
                </Table>
                <TrafficPagination
                  page={callerPage}
                  pageSize={pageSize}
                  total={callersQuery.data?.total_groups ?? 0}
                  onPageChange={setCallerPage}
                />
              </>
            )}
          </CardContent>
        </Card>
      </div>

      <TrafficDetailSheet
        detail={detail}
        onClose={() => setDetail(null)}
        data={detailQuery.data}
        isPending={detailQuery.isPending}
        isError={detailQuery.isError}
        page={detailPage}
        pageSize={pageSize}
        sort={detailSort}
        projectId={project.id}
        startDate={startIso ?? ''}
        endDate={endIso ?? ''}
        environmentId={selectedEnvironment}
        canOpenProxyLogs={proxyLogAccessQuery.data?.allowed === true}
        proxyLogAccessReason={
          proxyLogAccessQuery.data?.reason ??
          (proxyLogAccessQuery.isLoading
            ? 'Checking proxy-log access…'
            : 'Proxy-log access is unavailable')
        }
        onPageChange={setDetailPage}
        onSort={(metric) => {
          setDetailSort((current) => nextTrafficSort(current, metric))
          setDetailPage(0)
        }}
      />
    </div>
  )
}

function SortableTrafficHead({
  label,
  metric,
  active,
  onSort,
  className,
}: {
  label: string
  metric: TrafficMetric
  active: TrafficSort<TrafficMetric>
  onSort: (metric: TrafficMetric) => void
  className?: string
}) {
  return (
    <TableHead className={`text-right ${className ?? ''}`}>
      <button
        type="button"
        className={`ml-auto flex items-center gap-1 ${active.metric === metric ? 'text-foreground' : ''}`}
        onClick={() => onSort(metric)}
        aria-label={`Sort by ${label} ${active.metric === metric && active.direction === 'desc' ? 'ascending' : 'descending'}`}
      >
        {label}
        {active.metric !== metric ? (
          <ArrowUpDown className="h-3 w-3" />
        ) : active.direction === 'asc' ? (
          <ArrowUp className="h-3 w-3" />
        ) : (
          <ArrowDown className="h-3 w-3" />
        )}
      </button>
    </TableHead>
  )
}

function TrafficDetailSheet({
  detail,
  onClose,
  data,
  isPending,
  isError,
  page,
  pageSize,
  sort,
  projectId,
  startDate,
  endDate,
  environmentId,
  canOpenProxyLogs,
  proxyLogAccessReason,
  onPageChange,
  onSort,
}: {
  detail: TrafficDetail | null
  onClose: () => void
  data: TrafficAggregationResponse | undefined
  isPending: boolean
  isError: boolean
  page: number
  pageSize: number
  sort: TrafficSort<TrafficMetric>
  projectId: number
  startDate: string
  endDate: string
  environmentId?: number
  canOpenProxyLogs: boolean
  proxyLogAccessReason: string
  onPageChange: (page: number) => void
  onSort: (metric: TrafficMetric) => void
}) {
  return (
    <Sheet open={detail !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-5xl">
        <SheetHeader>
          <SheetTitle className="font-mono text-base">
            {detail?.kind === 'ip'
              ? `Routes called by ${detail.value}`
              : `IPs calling ${detail?.method ?? ''} ${detail?.value ?? ''}`}
          </SheetTitle>
          <SheetDescription>
            Analytics use the active dashboard time and environment filters.
            Temps monitor checks are excluded.
            {!canOpenProxyLogs && ` ${proxyLogAccessReason}.`}
          </SheetDescription>
        </SheetHeader>
        <div className="mt-6">
          {isPending ? (
            <Skeleton className="h-[360px] w-full" />
          ) : isError ? (
            <QueryErrorState label="traffic detail" />
          ) : (data?.rows.length ?? 0) === 0 ? (
            <>
              <p className="text-sm text-muted-foreground">
                No matching traffic on this page.
              </p>
              <TrafficPagination
                page={page}
                pageSize={pageSize}
                total={data?.total_groups ?? 0}
                onPageChange={onPageChange}
              />
            </>
          ) : (
            <>
              <div className="mb-4 grid grid-cols-2 gap-3 sm:grid-cols-4">
                <StatTile
                  label="Groups"
                  value={formatNumber(data?.total_groups ?? 0)}
                  loading={false}
                  error={false}
                />
                <StatTile
                  label="Page requests"
                  value={formatNumber(
                    data?.rows.reduce(
                      (sum, row) => sum + (row.metrics.requests ?? 0),
                      0
                    ) ?? 0
                  )}
                  loading={false}
                  error={false}
                />
                <StatTile
                  label="Page errors"
                  value={formatNumber(
                    data?.rows.reduce(
                      (sum, row) => sum + (row.metrics.errors ?? 0),
                      0
                    ) ?? 0
                  )}
                  loading={false}
                  error={false}
                />
                <StatTile
                  label="Monitor"
                  value="Excluded"
                  loading={false}
                  error={false}
                />
              </div>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>
                      {detail?.kind === 'ip' ? 'Route' : 'Client IP'}
                    </TableHead>
                    <SortableTrafficHead
                      label="Requests"
                      metric="requests"
                      active={sort}
                      onSort={onSort}
                    />
                    <SortableTrafficHead
                      label="Min"
                      metric="latency_min"
                      active={sort}
                      onSort={onSort}
                    />
                    <SortableTrafficHead
                      label="Avg"
                      metric="latency_avg"
                      active={sort}
                      onSort={onSort}
                    />
                    <SortableTrafficHead
                      label="Max"
                      metric="latency_max"
                      active={sort}
                      onSort={onSort}
                    />
                    <SortableTrafficHead
                      label="p95"
                      metric="latency_p95"
                      active={sort}
                      onSort={onSort}
                    />
                    <SortableTrafficHead
                      label="Err %"
                      metric="error_rate"
                      active={sort}
                      onSort={onSort}
                    />
                    <SortableTrafficHead
                      label="Last seen"
                      metric="last_seen"
                      active={sort}
                      onSort={onSort}
                    />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {data?.rows.map((row, index) => {
                    const clientIp =
                      detail?.kind === 'ip'
                        ? detail.value
                        : trafficDimension(row, 'client_ip')
                    const method =
                      detail?.kind === 'ip'
                        ? trafficDimension(row, 'method')
                        : (detail?.method ?? '')
                    const path =
                      detail?.kind === 'ip'
                        ? trafficDimension(row, 'path')
                        : (detail?.value ?? '')
                    const logsUrl = apiTrafficProxyLogsUrl({
                      projectId,
                      clientIp,
                      method,
                      path,
                      startDate,
                      endDate,
                      environmentId,
                    })
                    return (
                      <TableRow
                        key={`${row.dimensions.map((item) => item.value).join(':')}-${index}`}
                      >
                        <TableCell className="max-w-[300px]">
                          <div className="flex items-center gap-2">
                            {canOpenProxyLogs ? (
                              <Link
                                to={logsUrl}
                                className="truncate font-mono text-xs underline-offset-4 hover:underline"
                                title="View matching request logs"
                              >
                                {detail?.kind === 'ip'
                                  ? `${method} ${path}`
                                  : clientIp}
                              </Link>
                            ) : (
                              <span
                                className="truncate font-mono text-xs"
                                title={proxyLogAccessReason}
                              >
                                {detail?.kind === 'ip'
                                  ? `${method} ${path}`
                                  : clientIp}
                              </span>
                            )}
                            <CrawlerSignals row={row} />
                          </div>
                        </TableCell>
                        <TableCell className="text-right">
                          {formatNumber(row.metrics.requests ?? 0)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatMs(row.metrics.latency_min_ms)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatMs(row.metrics.latency_avg_ms)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatMs(row.metrics.latency_max_ms)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatMs(row.metrics.latency_p95_ms)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatPercent(row.metrics.error_rate ?? 0)}
                        </TableCell>
                        <TableCell className="text-right text-xs text-muted-foreground">
                          {row.metrics.last_seen
                            ? new Date(row.metrics.last_seen).toLocaleString()
                            : '—'}
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
              <TrafficPagination
                page={page}
                pageSize={pageSize}
                total={data?.total_groups ?? 0}
                onPageChange={onPageChange}
              />
            </>
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}

function TrafficPagination({
  page,
  pageSize,
  total,
  onPageChange,
}: {
  page: number
  pageSize: number
  total: number
  onPageChange: (page: number) => void
}) {
  const pageCount = trafficPageCount(total, pageSize)
  if (pageCount <= 1) return null
  return (
    <div className="mt-3 flex items-center justify-between border-t pt-3">
      <span className="text-xs text-muted-foreground">
        Page {page + 1} of {pageCount} · {formatNumber(total)} total
      </span>
      <div className="flex gap-1">
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => onPageChange(Math.max(0, page - 1))}
          disabled={page === 0}
          aria-label="Previous page"
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => onPageChange(Math.min(pageCount - 1, page + 1))}
          disabled={page + 1 >= pageCount}
          aria-label="Next page"
        >
          <ChevronRight className="h-4 w-4" />
        </Button>
      </div>
    </div>
  )
}

function CrawlerSignals({ row }: { row: TrafficAggregationRow }) {
  const botRequests = row.metrics.bot_requests ?? 0
  const robotsTxtRequests = row.metrics.robots_txt_requests ?? 0
  const requests = row.metrics.requests ?? 0
  const entirelyCrawlerTraffic = requests > 0 && botRequests === requests
  if (botRequests === 0 && robotsTxtRequests === 0) return null

  return (
    <div className="flex shrink-0 items-center gap-1">
      {botRequests > 0 && (
        <Badge
          variant="secondary"
          className="px-1.5 py-0 text-[10px]"
          title={`${formatNumber(botRequests)} request${botRequests === 1 ? '' : 's'} classified as bot traffic from user-agent metadata`}
        >
          {entirelyCrawlerTraffic ? 'Crawler' : 'Bot traffic'}
        </Badge>
      )}
      {robotsTxtRequests > 0 && (
        <Badge
          variant="outline"
          className="px-1.5 py-0 text-[10px]"
          title={`${formatNumber(robotsTxtRequests)} request${robotsTxtRequests === 1 ? '' : 's'} to /robots.txt`}
        >
          robots.txt
        </Badge>
      )}
    </div>
  )
}

function StatTile({
  label,
  value,
  loading,
  error,
}: {
  label: string
  value: string
  loading: boolean
  error: boolean
}) {
  return (
    <Card>
      <CardContent className="p-4">
        <p className="text-xs text-muted-foreground">{label}</p>
        {loading ? (
          <Skeleton className="mt-1 h-7 w-16" />
        ) : error ? (
          <p className="mt-1 text-sm font-medium text-destructive">
            Unavailable
          </p>
        ) : (
          <p className="mt-1 text-2xl font-semibold">{value}</p>
        )}
      </CardContent>
    </Card>
  )
}

function QueryErrorState({ label }: { label: string }) {
  return (
    <div className="rounded-md border border-destructive/30 bg-destructive/5 p-4 text-sm text-destructive">
      Could not load {label}. Refresh to try again.
    </div>
  )
}

function ApiTrafficSummaryCard({
  project,
  data,
  error,
  cached,
  enabled,
  requested,
  canGenerate,
  isPending,
  onGenerate,
  onRefresh,
}: {
  project: ProjectResponse
  data: ApiTrafficAiSummary | undefined
  error: string | undefined
  cached: boolean
  enabled: boolean
  requested: boolean
  canGenerate: boolean
  isPending: boolean
  onGenerate: () => void
  onRefresh: () => void
}) {
  const queryClient = useQueryClient()
  const enableMutation = useMutation({
    ...updateProjectSettingsMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: getProjectBySlugQueryKey({
          path: { slug: project.slug },
        }),
      })
      toast.success('AI traffic summaries enabled')
    },
    onError: (mutationError) =>
      toast.error('Could not enable AI traffic summaries', {
        description: String(mutationError),
      }),
  })

  // Rendered unconditionally per the project's feature-discoverability rule:
  // an unconfigured AI summary must onboard here, not disappear from the page.
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Sparkles className="h-4 w-4" />
          AI traffic summary
          {cached && <Badge variant="secondary">Cached</Badge>}
        </CardTitle>
      </CardHeader>
      <CardContent>
        {enabled && requested && (isPending || data) ? (
          <div className="space-y-3">
            {data?.headline ? (
              <p className="font-medium">{data.headline}</p>
            ) : (
              <Skeleton className="h-5 w-3/4" />
            )}
            {data?.findings ? (
              <ul className="list-inside list-disc space-y-1 text-sm text-muted-foreground">
                {data.findings.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            ) : (
              <div className="space-y-2">
                <Skeleton className="h-4 w-full" />
                <Skeleton className="h-4 w-5/6" />
              </div>
            )}
            {data?.anomalies ? (
              data.anomalies.length > 0 && (
                <div className="space-y-1 rounded-md border border-amber-500/30 bg-amber-500/5 p-3">
                  <p className="flex items-center gap-1.5 text-sm font-medium text-amber-600 dark:text-amber-400">
                    <AlertTriangle className="h-3.5 w-3.5" />
                    Anomalies
                  </p>
                  <ul className="list-inside list-disc space-y-1 text-sm text-muted-foreground">
                    {data.anomalies.map((a, i) => (
                      <li key={i}>{a}</li>
                    ))}
                  </ul>
                </div>
              )
            ) : (
              <Skeleton className="h-16 w-full" />
            )}
            {data && 'recommendation' in data ? (
              data.recommendation && (
                <p className="text-sm">
                  <span className="font-medium">Recommendation: </span>
                  {data.recommendation}
                </p>
              )
            ) : (
              <Skeleton className="h-4 w-2/3" />
            )}
            {data && !isPending && (
              <Button size="sm" variant="outline" onClick={onRefresh}>
                Refresh summary
              </Button>
            )}
          </div>
        ) : (
          <div className="flex flex-col items-start gap-2 rounded-md border border-dashed p-4">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Server className="h-4 w-4 text-muted-foreground" />
              {!enabled
                ? 'No summary available'
                : error
                  ? 'Summary unavailable'
                  : 'Generate a summary when you need one'}
            </div>
            <p className="text-sm text-muted-foreground">
              {error ??
                (!enabled
                  ? "AI summaries analyze this period's traffic and call out anomalies and error spikes."
                  : 'AI runs only when you request it. Cached results are reused for 15 minutes; refreshing the summary explicitly bypasses that cache.')}
            </p>
            <div className="flex flex-wrap gap-2">
              {!enabled && (
                <Button
                  size="sm"
                  disabled={enableMutation.isPending}
                  onClick={() =>
                    enableMutation.mutate({
                      path: { project_id: project.id },
                      body: { ai_api_traffic_summary_enabled: true },
                    })
                  }
                >
                  {enableMutation.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Enable AI summary
                </Button>
              )}
              {enabled && (
                <Button
                  size="sm"
                  disabled={!canGenerate || isPending}
                  onClick={requested ? onRefresh : onGenerate}
                >
                  {isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  {error ? 'Try again' : 'Generate summary'}
                </Button>
              )}
              <Button size="sm" variant="outline" asChild>
                <Link to="/ai-gateway">Configure AI provider</Link>
              </Button>
            </div>
            {!enabled && (
              <Link
                to={`/projects/${project.slug}/settings/security`}
                className="text-xs text-muted-foreground underline underline-offset-4"
              >
                View all project AI settings
              </Link>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  )
}
