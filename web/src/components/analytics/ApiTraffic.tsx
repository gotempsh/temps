import {
  getApiCallersOptions,
  getApiRoutesOptions,
  getApiSummaryOptions,
  getApiTimeseriesOptions,
  getProjectBySlugQueryKey,
  getProjectDeploymentsOptions,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { getApiSummary } from '@/api/client'
import { ProjectResponse } from '@/api/client/types.gen'
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
import { shouldRequestApiTrafficSummary } from '@/lib/ai-summary'

interface ApiTrafficTabProps {
  project: ProjectResponse
}

interface ApiTrafficAiSummary {
  headline?: string
  findings?: string[]
  anomalies?: string[]
  recommendation?: string | null
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

export function apiTrafficSummaryCacheKey({
  environmentId,
  filter,
  startIso,
  endIso,
}: {
  environmentId?: number
  filter: QuickFilter
  startIso?: string
  endIso?: string
}): string {
  const environment = environmentId ?? 'all'
  if (filter === 'custom') {
    return `api-traffic:v2:${environment}:custom:${startIso}:${endIso}`
  }
  return `api-traffic:v2:${environment}:${filter}`
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
        return !!id && /getApi(Timeseries|Routes|Callers)/.test(id)
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
    ...getApiRoutesOptions({
      path: { project_id: project.id },
      query: {
        start_date: startIso ?? '',
        end_date: endIso ?? '',
        environment_id: selectedEnvironment,
        limit: pageSize,
        offset: routePage * pageSize,
      },
    }),
    enabled,
  })

  const callersQuery = useQuery({
    ...getApiCallersOptions({
      path: { project_id: project.id },
      query: {
        start_date: startIso ?? '',
        end_date: endIso ?? '',
        environment_id: selectedEnvironment,
        limit: pageSize,
        offset: callerPage * pageSize,
      },
    }),
    enabled,
  })

  // Stable first-page context for the summary. React Query deduplicates these
  // with the visible queries on page 1 and retains them when the user pages
  // through the tables, preventing pagination from triggering new AI calls.
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
    enabled,
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
    enabled,
  })

  // Wait for the visible aggregates before requesting the summary. The
  // backend keeps a tiny, short-lived cache keyed by this exact window, so
  // the summary reuses these results instead of repeating three proxy_logs
  // scans while the first requests are still in flight.
  const summaryDataReady =
    enabled &&
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
        canGenerate={summaryDataReady}
        isPending={
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
              emptyMessage="No API traffic in this period."
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
              routes with dynamic IDs may appear as separate rows.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {routesQuery.isPending ? (
              <Skeleton className="h-[240px] w-full" />
            ) : routesQuery.isError ? (
              <QueryErrorState label="route data" />
            ) : (routesQuery.data?.routes.length ?? 0) === 0 ? (
              <p className="text-sm text-muted-foreground">
                No API traffic for this period.
              </p>
            ) : (
              <>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Route</TableHead>
                      <TableHead className="text-right">Requests</TableHead>
                      <TableHead className="text-right hidden sm:table-cell">
                        Avg
                      </TableHead>
                      <TableHead className="text-right">Err %</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {routesQuery.data?.routes.map((r, i) => (
                      <TableRow key={`${r.method}-${r.path}-${i}`}>
                        <TableCell className="max-w-[220px]">
                          <div className="flex items-center gap-2">
                            <Badge variant="outline" className="shrink-0">
                              {r.method}
                            </Badge>
                            <span className="truncate font-mono text-xs">
                              {r.path}
                            </span>
                          </div>
                        </TableCell>
                        <TableCell className="text-right">
                          {formatNumber(r.request_count)}
                        </TableCell>
                        <TableCell className="text-right hidden sm:table-cell">
                          {formatMs(r.avg_latency_ms)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatPercent(r.error_rate)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                <TrafficPagination
                  page={routePage}
                  pageSize={pageSize}
                  total={routesQuery.data?.total_routes ?? 0}
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
              Client IPs ranked by request count in this window.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {callersQuery.isPending ? (
              <Skeleton className="h-[240px] w-full" />
            ) : callersQuery.isError ? (
              <QueryErrorState label="caller data" />
            ) : (callersQuery.data?.callers.length ?? 0) === 0 ? (
              <p className="text-sm text-muted-foreground">
                No API traffic for this period.
              </p>
            ) : (
              <>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Client IP</TableHead>
                      <TableHead className="text-right">Requests</TableHead>
                      <TableHead className="text-right">Err %</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {callersQuery.data?.callers.map((c) => (
                      <TableRow key={c.client_ip}>
                        <TableCell className="font-mono text-xs">
                          {c.client_ip}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatNumber(c.request_count)}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatPercent(c.error_rate)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                <TrafficPagination
                  page={callerPage}
                  pageSize={pageSize}
                  total={callersQuery.data?.total_callers ?? 0}
                  onPageChange={setCallerPage}
                />
              </>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
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
  const pageCount = Math.max(1, Math.ceil(total / pageSize))
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
