import { LogRecord, LogSeverity, ProjectResponse } from '@/api/client'
import { queryLogsOptions } from '@/api/client/@tanstack/react-query.gen'
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
import { useAssistantPageContext } from '@/components/ai/AiAssistantContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useDebounce } from '@/hooks/useDebounce'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ChevronLeft,
  ChevronRight,
  Clock,
  Network,
  RefreshCw,
  ScrollText,
  Search,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'

interface LogsListProps {
  project: ProjectResponse
}

type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

const PAGE_SIZE = 50
const ALL = '__all__'
const SEVERITIES: LogSeverity[] = [
  'TRACE',
  'DEBUG',
  'INFO',
  'WARN',
  'ERROR',
  'FATAL',
]

// Severity → badge. ERROR/FATAL are loud, WARN amber, INFO neutral, DEBUG/TRACE muted.
function severityBadge(severity: LogSeverity, text?: string) {
  const label = text || severity
  switch (severity) {
    case 'FATAL':
    case 'ERROR':
      return <Badge variant="destructive">{label}</Badge>
    case 'WARN':
      return <Badge variant="warning">{label}</Badge>
    case 'INFO':
      return <Badge variant="secondary">{label}</Badge>
    default:
      return (
        <Badge variant="outline" className="text-muted-foreground">
          {label}
        </Badge>
      )
  }
}

export default function LogsList({ project }: LogsListProps) {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle(`Logs - ${project.name}`)

  const [timeRange, setTimeRange] = useState<TimeRange>(
    () => (searchParams.get('range') as TimeRange) || '24h',
  )
  const [severity, setSeverity] = useState<string>(
    () => searchParams.get('severity') || ALL,
  )
  const [service, setService] = useState(() => searchParams.get('service') || '')
  const [search, setSearch] = useState(() => searchParams.get('q') || '')
  // Trace correlation: when arriving from a trace, `?trace=<id>` pins the filter.
  const traceId = searchParams.get('trace') || ''
  const [page, setPage] = useState(1)
  const [expanded, setExpanded] = useState<Set<number>>(new Set())

  const debouncedService = useDebounce(service, 300)
  const debouncedSearch = useDebounce(search, 300)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.name, href: `/projects/${project.slug}` },
      { label: 'Logs' },
    ])
  }, [project.name, project.slug, setBreadcrumbs])

  // Reset to first page when any filter changes.
  useEffect(() => {
    setPage(1)
    setExpanded(new Set())
  }, [timeRange, severity, debouncedService, debouncedSearch, traceId])

  // Persist filters to the URL.
  useEffect(() => {
    const params = new URLSearchParams()
    if (timeRange !== '24h') params.set('range', timeRange)
    if (severity !== ALL) params.set('severity', severity)
    if (debouncedService) params.set('service', debouncedService)
    if (debouncedSearch) params.set('q', debouncedSearch)
    if (traceId) params.set('trace', traceId)
    setSearchParams(params, { replace: true })
  }, [timeRange, severity, debouncedService, debouncedSearch, traceId, setSearchParams])

  const { startTime, endTime } = useMemo(() => {
    const now = new Date()
    const start = new Date()
    switch (timeRange) {
      case '1h':
        start.setHours(start.getHours() - 1)
        break
      case '6h':
        start.setHours(start.getHours() - 6)
        break
      case '24h':
        start.setDate(start.getDate() - 1)
        break
      case '7d':
        start.setDate(start.getDate() - 7)
        break
      case '30d':
        start.setDate(start.getDate() - 30)
        break
    }
    return { startTime: start.toISOString(), endTime: now.toISOString() }
  }, [timeRange])

  const { data, isLoading, isFetching, refetch } = useQuery({
    ...queryLogsOptions({
      query: {
        project_id: project.id,
        severity: severity !== ALL ? severity : undefined,
        service_name: debouncedService || undefined,
        search: debouncedSearch || undefined,
        trace_id: traceId || undefined,
        // When pinned to a trace, ignore the time window — a trace's logs may
        // predate the selected range, and the trace_id is already specific.
        start_time: traceId ? undefined : startTime,
        end_time: traceId ? undefined : endTime,
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
      },
    }),
  })

  const logs: LogRecord[] = data?.data ?? []
  const hasMore = logs.length === PAGE_SIZE
  const hasFilters = severity !== ALL || !!service || !!search

  // Tell the assistant the user is exploring logs (+ active filters).
  const assistantContext = useMemo(() => {
    const filters = [
      traceId && `trace ${traceId}`,
      severity !== ALL && `severity ${severity}`,
      service && `service "${service}"`,
      search && `search "${search}"`,
    ].filter(Boolean)
    return [
      'The user is in the OpenTelemetry Logs explorer in the Temps console.',
      `Project: "${project.name}" (slug: ${project.slug}, id: ${project.id}).`,
      `Time range: ${timeRange}.${filters.length ? ` Active filters: ${filters.join(', ')}.` : ''}`,
      'Fetch logs with the temps CLI: `telemetry-logs query_logs --severity/--service_name/--search/--trace_id`.',
    ].join('\n')
  }, [project, timeRange, severity, service, search, traceId])
  useAssistantPageContext(assistantContext, 'these logs')

  const toggleExpand = (i: number) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(i)) next.delete(i)
      else next.add(i)
      return next
    })
  }

  const clearTrace = () => {
    const params = new URLSearchParams(searchParams)
    params.delete('trace')
    setSearchParams(params, { replace: true })
  }

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Logs</h2>
          <p className="text-sm text-muted-foreground">
            Structured logs from your application via OpenTelemetry
          </p>
        </div>
        <Button
          variant="ghost"
          size="icon"
          onClick={() => refetch()}
          disabled={isFetching}
        >
          <RefreshCw className={`h-4 w-4 ${isFetching ? 'animate-spin' : ''}`} />
        </Button>
      </div>

      {/* Trace-correlation banner */}
      {traceId && (
        <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2 text-sm">
          <Network className="h-4 w-4 shrink-0 text-muted-foreground" />
          <span>
            Showing logs for trace{' '}
            <code className="font-mono text-xs">{traceId.slice(0, 24)}…</code>
          </span>
          <Button
            variant="ghost"
            size="sm"
            className="ml-auto h-7"
            onClick={() => navigate(`../traces/${traceId}`)}
          >
            View trace
          </Button>
          <Button variant="ghost" size="icon" className="h-7 w-7" onClick={clearTrace}>
            <X className="h-3.5 w-3.5" />
          </Button>
        </div>
      )}

      {/* Filters */}
      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
            <Select
              value={timeRange}
              onValueChange={(v) => setTimeRange(v as TimeRange)}
              disabled={!!traceId}
            >
              <SelectTrigger className="w-full sm:w-[140px]">
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

            <Select value={severity} onValueChange={setSeverity}>
              <SelectTrigger className="w-full sm:w-[150px]">
                <SelectValue placeholder="Severity" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>All severities</SelectItem>
                {SEVERITIES.map((s) => (
                  <SelectItem key={s} value={s}>
                    {s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            <Input
              placeholder="Service name…"
              value={service}
              onChange={(e) => setService(e.target.value)}
              className="h-9 w-full sm:w-[180px]"
            />

            <div className="relative flex-1 min-w-0 sm:min-w-[220px]">
              <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder="Search log message…"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                className="h-9 pl-8"
              />
            </div>
          </div>
        </CardContent>
      </Card>

      {/* List */}
      {isLoading ? (
        <div className="space-y-2">
          {Array.from({ length: 10 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      ) : logs.length === 0 ? (
        <EmptyState
          icon={ScrollText}
          title="No logs found"
          description={
            hasFilters || traceId
              ? 'Try adjusting your filters or time range.'
              : 'Logs will appear here once your application sends them via OpenTelemetry (OTLP).'
          }
        />
      ) : (
        <>
          <div className="overflow-hidden rounded-md border font-mono text-xs">
            {logs.map((log, i) => {
              const isOpen = expanded.has(i)
              return (
                <div key={i} className="border-b last:border-b-0">
                  <button
                    type="button"
                    onClick={() => toggleExpand(i)}
                    className="flex w-full items-start gap-3 px-3 py-1.5 text-left hover:bg-muted/50"
                  >
                    <span className="shrink-0 text-muted-foreground tabular-nums">
                      {format(new Date(log.timestamp), 'MMM d HH:mm:ss.SSS')}
                    </span>
                    <span className="shrink-0">
                      {severityBadge(log.severity, log.severity_text)}
                    </span>
                    <span className="shrink-0 max-w-[140px] truncate text-muted-foreground">
                      {log.resource?.service_name || '—'}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-foreground">
                      {log.body}
                    </span>
                    {log.trace_id && (
                      <Network className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                    )}
                  </button>

                  {isOpen && (
                    <div className="space-y-2 bg-muted/30 px-3 py-2">
                      <pre className="whitespace-pre-wrap break-words text-foreground">
                        {log.body}
                      </pre>
                      <div className="flex flex-wrap gap-x-4 gap-y-1 text-muted-foreground">
                        <span>service: {log.resource?.service_name || '—'}</span>
                        {log.resource?.deployment_environment && (
                          <span>env: {log.resource.deployment_environment}</span>
                        )}
                        {log.resource?.service_version && (
                          <span>version: {log.resource.service_version}</span>
                        )}
                        <span>severity: {log.severity_text || log.severity}</span>
                        {log.trace_id && (
                          <button
                            type="button"
                            onClick={() => navigate(`../traces/${log.trace_id}`)}
                            className="text-primary hover:underline"
                          >
                            view trace ↗
                          </button>
                        )}
                      </div>
                      {Object.keys(log.attributes || {}).length > 0 && (
                        <div className="space-y-0.5">
                          {Object.entries(log.attributes).map(([k, v]) => (
                            <div key={k} className="flex gap-2">
                              <span className="shrink-0 text-muted-foreground">
                                {k}
                              </span>
                              <span className="break-all text-foreground">
                                {String(v)}
                              </span>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )
            })}
          </div>

          {/* Pagination — the logs endpoint returns a page, not a grand total. */}
          <div className="flex items-center justify-between">
            <span className="text-sm text-muted-foreground">
              Page {page}
              {logs.length > 0 && ` · ${logs.length} line${logs.length === 1 ? '' : 's'}`}
            </span>
            <div className="flex gap-1">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={page === 1 || isFetching}
              >
                <ChevronLeft className="h-4 w-4" />
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage((p) => p + 1)}
                disabled={!hasMore || isFetching}
              >
                <ChevronRight className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </>
      )}
    </div>
  )
}
