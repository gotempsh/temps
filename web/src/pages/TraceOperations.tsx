import { EnvironmentResponse, ProjectResponse, SpanStats } from '@/api/client'
import {
  getEnvironmentsOptions,
  querySpanStatsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
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
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useDebounce } from '@/hooks/useDebounce'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  computeTracesTimeWindow,
  type TracesTimeRange,
} from '@/lib/traces-time-window'
import { useQuery } from '@tanstack/react-query'
import {
  ChevronLeft,
  ChevronRight,
  Clock,
  Gauge,
  RefreshCw,
  Search,
  Workflow,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router'

interface TraceOperationsProps {
  project: ProjectResponse
}

const PAGE_SIZE = 25

/**
 * Ranking options, in the order an operator usually reaches for them.
 *
 * Each answers a different question, which is why the sort is a first-class
 * control rather than a column header: "where does the time go" and "which
 * operation is unpredictable" have completely different answers on the same
 * data.
 */
const SORT_OPTIONS: { value: string; label: string; hint: string }[] = [
  {
    value: 'total_time',
    label: 'Total time spent',
    hint: 'Where the wall-clock actually goes — cost x volume',
  },
  { value: 'p95', label: 'Slowest (p95)', hint: 'What most users feel' },
  { value: 'p99', label: 'Slowest (p99)', hint: 'What your worst-off users feel' },
  { value: 'max', label: 'Worst single call', hint: 'The slowest this ever got' },
  {
    value: 'variability',
    label: 'Most inconsistent',
    hint: 'Spread relative to the average (stddev / avg)',
  },
  {
    value: 'tail_ratio',
    label: 'Worst tail (p99/p50)',
    hint: 'How much worse the bad case is than the typical one',
  },
  { value: 'count', label: 'Most called', hint: 'Highest call volume' },
  { value: 'error_rate', label: 'Highest error rate', hint: 'Most failure-prone' },
]

/**
 * Minimum samples required before an operation is ranked.
 *
 * The variability rankings are ratios, and a ratio computed from three calls
 * is noise that outranks every real signal. Defaulting to 10 keeps one-off
 * spans out of the report; the control stays visible so it can be lowered.
 */
const MIN_COUNT_OPTIONS = ['1', '10', '50', '100', '500']

function formatMs(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  if (ms >= 1) return `${ms.toFixed(1)}ms`
  return `${ms.toFixed(2)}ms`
}

/** Total wall-clock is usually minutes or hours, not milliseconds. */
function formatTotal(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  if (ms >= 3_600_000) return `${(ms / 3_600_000).toFixed(1)}h`
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}m`
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`
  return `${ms.toFixed(0)}ms`
}

/**
 * A tail ratio above 10 means the bad case is an order of magnitude worse than
 * the typical one — the signal this page exists to surface, so it gets colour.
 */
function tailRatioBadge(ratio: number) {
  if (ratio <= 0) return <span className="text-muted-foreground">—</span>
  const label = `${ratio.toFixed(1)}x`
  if (ratio >= 10) return <Badge variant="destructive">{label}</Badge>
  if (ratio >= 4) return <Badge variant="warning">{label}</Badge>
  return <span className="text-muted-foreground">{label}</span>
}

export default function TraceOperations({ project }: TraceOperationsProps) {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()

  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle(`Operations - ${project.name}`)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.name, href: `/projects/${project.slug}` },
      { label: 'Traces', href: `/projects/${project.slug}/traces` },
      { label: 'Operations' },
    ])
  }, [project.name, project.slug, setBreadcrumbs])

  const [timeRange, setTimeRange] = useState<TracesTimeRange>(
    (searchParams.get('range') as TracesTimeRange) || '24h'
  )
  const [sortBy, setSortBy] = useState(searchParams.get('sort') || 'total_time')
  const [minCount, setMinCount] = useState(searchParams.get('min_count') || '10')
  const [environmentId, setEnvironmentId] = useState(
    searchParams.get('environment') || 'all'
  )
  const [search, setSearch] = useState(searchParams.get('q') || '')
  const [page, setPage] = useState(1)
  // Bumping this recomputes the relative window against a fresh `now`; freezing
  // it at mount would silently exclude everything ingested since page load.
  const [refreshKey, setRefreshKey] = useState(0)

  const debouncedSearch = useDebounce(search, 300)

  const timeWindow = useMemo(
    () => computeTracesTimeWindow(timeRange, new Date()),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [timeRange, refreshKey]
  )

  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })

  const { data, isLoading, isFetching, isError, error, refetch } = useQuery({
    ...querySpanStatsOptions({
      query: {
        project_id: project.id,
        start_time: timeWindow.startTime,
        end_time: timeWindow.endTime,
        sort_by: sortBy,
        min_count: Number(minCount),
        ...(debouncedSearch ? { name_pattern: debouncedSearch } : {}),
        ...(environmentId !== 'all'
          ? { environment_id: Number(environmentId) }
          : {}),
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
      },
    }),
    enabled: !!project.id,
  })

  const rows: SpanStats[] = data?.data ?? []
  const totalCount = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE))

  const updateParam = useCallback(
    (key: string, value: string, fallback: string) => {
      const next = new URLSearchParams(searchParams)
      if (value === fallback) next.delete(key)
      else next.set(key, value)
      setSearchParams(next, { replace: true })
    },
    [searchParams, setSearchParams]
  )

  const handleSortChange = useCallback(
    (v: string) => {
      setSortBy(v)
      setPage(1)
      updateParam('sort', v, 'total_time')
    },
    [updateParam]
  )
  const handleRangeChange = useCallback(
    (v: string) => {
      setTimeRange(v as TracesTimeRange)
      setPage(1)
      updateParam('range', v, '24h')
    },
    [updateParam]
  )
  const handleMinCountChange = useCallback(
    (v: string) => {
      setMinCount(v)
      setPage(1)
      updateParam('min_count', v, '10')
    },
    [updateParam]
  )
  const handleEnvironmentChange = useCallback(
    (v: string) => {
      setEnvironmentId(v)
      setPage(1)
      updateParam('environment', v, 'all')
    },
    [updateParam]
  )

  const activeSort = SORT_OPTIONS.find((o) => o.value === sortBy)

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Operations</h2>
          <p className="text-sm text-muted-foreground">
            Which operations cost the most time — and which are unpredictable
          </p>
        </div>
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => {
              setRefreshKey((k) => k + 1)
              void refetch()
            }}
            disabled={isFetching}
          >
            <RefreshCw
              className={`h-4 w-4 ${isFetching ? 'animate-spin' : ''}`}
            />
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="gap-2"
            onClick={() => navigate(`/projects/${project.slug}/traces`)}
          >
            <Workflow className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">All Traces</span>
          </Button>
          {totalCount > 0 && (
            <span className="text-sm text-muted-foreground">
              {totalCount.toLocaleString()} operation
              {totalCount !== 1 ? 's' : ''}
            </span>
          )}
        </div>
      </div>

      {/* Filters */}
      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
            <Select value={sortBy} onValueChange={handleSortChange}>
              <SelectTrigger className="w-full sm:w-[220px]">
                <Gauge className="mr-2 h-3.5 w-3.5" />
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SORT_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            <Select value={timeRange} onValueChange={handleRangeChange}>
              <SelectTrigger className="w-full sm:w-[150px]">
                <Clock className="mr-2 h-3.5 w-3.5" />
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="1h">Last 1 hour</SelectItem>
                <SelectItem value="6h">Last 6 hours</SelectItem>
                <SelectItem value="24h">Last 24 hours</SelectItem>
                <SelectItem value="7d">Last 7 days</SelectItem>
                <SelectItem value="30d">Last 30 days</SelectItem>
              </SelectContent>
            </Select>

            <Select value={minCount} onValueChange={handleMinCountChange}>
              <SelectTrigger className="w-full sm:w-[160px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {MIN_COUNT_OPTIONS.map((value) => (
                  <SelectItem key={value} value={value}>
                    Min {value} call{value === '1' ? '' : 's'}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            {environments && environments.length > 0 && (
              <Select
                value={environmentId}
                onValueChange={handleEnvironmentChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Environment" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Environments</SelectItem>
                  {environments.map((env: EnvironmentResponse) => (
                    <SelectItem key={env.id} value={String(env.id)}>
                      {env.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            <div className="relative flex-1 sm:min-w-[220px]">
              <Search className="absolute left-2.5 top-2.5 h-3.5 w-3.5 text-muted-foreground" />
              <Input
                placeholder="Filter by operation name..."
                value={search}
                onChange={(e) => {
                  setSearch(e.target.value)
                  setPage(1)
                }}
                className="pl-8"
              />
            </div>
          </div>
          {activeSort && (
            <p className="mt-2 text-xs text-muted-foreground">
              {activeSort.hint}
            </p>
          )}
        </CardContent>
      </Card>

      {/* Results */}
      <Card>
        <CardContent className="p-0">
          {isError ? (
            <div className="p-6">
              <EmptyState
                icon={Gauge}
                title="Could not load operations"
                description={
                  error instanceof Error
                    ? error.message
                    : 'The span-stats query failed. Try a narrower time range, or check the server logs.'
                }
                action={
                  <Button variant="outline" size="sm" onClick={() => refetch()}>
                    Retry
                  </Button>
                }
              />
            </div>
          ) : isLoading ? (
            <div className="space-y-2 p-4">
              {Array.from({ length: 8 }).map((_, i) => (
                <Skeleton key={i} className="h-10 w-full" />
              ))}
            </div>
          ) : rows.length === 0 ? (
            <div className="p-6">
              <EmptyState
                icon={Gauge}
                title="No operations to rank"
                description={
                  search || environmentId !== 'all'
                    ? 'No operations match these filters. Try clearing the name filter or environment.'
                    : minCount !== '1'
                      ? `No operation was called at least ${minCount} times in this window. Lower the minimum, or widen the time range.`
                      : 'No spans in this time range. Widen the range, or confirm your application is exporting traces via OpenTelemetry.'
                }
                action={
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => navigate(`/projects/${project.slug}/traces`)}
                  >
                    Go to Traces
                  </Button>
                }
              />
            </div>
          ) : (
            <div className="overflow-x-auto">
              <Table className="min-w-[900px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>Operation</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Service
                    </TableHead>
                    <TableHead className="text-right">Calls</TableHead>
                    <TableHead className="text-right">Errors</TableHead>
                    <TableHead className="text-right">Total</TableHead>
                    <TableHead className="text-right">p50</TableHead>
                    <TableHead className="text-right">p95</TableHead>
                    <TableHead className="hidden lg:table-cell text-right">
                      p99
                    </TableHead>
                    <TableHead className="text-right">Max</TableHead>
                    <TableHead className="text-right">
                      <Tooltip>
                        <TooltipTrigger className="cursor-help underline decoration-dotted underline-offset-4">
                          p99/p50
                        </TooltipTrigger>
                        <TooltipContent>
                          How much worse the bad case is than the typical one.
                          A high ratio means this operation is unpredictable,
                          even if its average looks fine.
                        </TooltipContent>
                      </Tooltip>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((row) => (
                    <TableRow
                      key={`${row.project_id}-${row.service_name}-${row.span_name}`}
                      className="cursor-pointer"
                      onClick={() =>
                        // Drill through to the traces that contain this
                        // operation, sorted slowest-first: the report says
                        // "this is slow", the list says "here is one".
                        navigate(
                          `/projects/${project.slug}/traces?name=${encodeURIComponent(
                            row.span_name
                          )}&service=${encodeURIComponent(row.service_name)}&sort=duration&range=${timeRange}`
                        )
                      }
                    >
                      <TableCell className="font-medium">
                        {row.span_name}
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-muted-foreground">
                        {row.service_name}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {row.count.toLocaleString()}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {row.error_count > 0 ? (
                          <span className="text-destructive">
                            {(row.error_rate * 100).toFixed(1)}%
                          </span>
                        ) : (
                          <span className="text-muted-foreground">—</span>
                        )}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {formatTotal(row.total_duration_ms)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {formatMs(row.p50_duration_ms)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {formatMs(row.p95_duration_ms)}
                      </TableCell>
                      <TableCell className="hidden lg:table-cell text-right tabular-nums">
                        {formatMs(row.p99_duration_ms)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {formatMs(row.max_duration_ms)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {tailRatioBadge(row.tail_ratio)}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Pagination */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between">
          <span className="text-sm text-muted-foreground">
            <span className="hidden sm:inline">
              Showing {(page - 1) * PAGE_SIZE + 1}–
              {Math.min(page * PAGE_SIZE, totalCount)} of {totalCount}
            </span>
            <span className="sm:hidden">
              {page} / {totalPages}
            </span>
          </span>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
            >
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}
