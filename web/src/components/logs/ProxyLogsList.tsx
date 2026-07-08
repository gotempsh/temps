import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getProxyLogsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AiAgentLogo } from '@/components/ui/ai-agent-logo'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
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
import { useQuery } from '@tanstack/react-query'
import { format, subDays, subHours } from 'date-fns'
import {
  ChevronLeft,
  ChevronRight,
  FileSearch,
  SlidersHorizontal,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

interface ProxyLogsListProps {
  project: ProjectResponse
  onRowClick?: (logId: number, projectId: number, timestamp: string) => void
  showEnvironmentFilter?: boolean
}

export default function ProxyLogsList({
  project,
  onRowClick,
  showEnvironmentFilter = true,
}: ProxyLogsListProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const [page, setPage] = useState<number>(() => {
    const pageParam = searchParams.get('page')
    return pageParam ? parseInt(pageParam) : 1
  })
  const [limit, setLimit] = useState<number>(() => {
    const limitParam = searchParams.get('limit')
    return limitParam ? parseInt(limitParam) : 25
  })
  const [method, setMethod] = useState<string>('all')
  const [statusCode, setStatusCode] = useState<string>(() => {
    const statusCode = searchParams.get('status_code')
    return statusCode ? statusCode : 'all'
  })
  const [timeRange, setTimeRange] = useState<string>(() => {
    const explicit = searchParams.get('time_range')
    if (explicit) return explicit
    // Drill-downs from analytics (AI agent/provider/page) default to a wider
    // 7-day window so the crawler rows they linked to actually fall inside it —
    // the analytics overview itself defaults to multi-day ranges.
    const fromAnalytics =
      searchParams.get('ai_agent') ||
      searchParams.get('ai_provider') ||
      searchParams.get('is_ai_agent') ||
      searchParams.get('path')
    return fromAnalytics ? '7d' : '24h'
  })
  const [environment, setEnvironment] = useState<string>(() => {
    return searchParams.get('environment') || 'all'
  })
  const [showBots, setShowBots] = useState<string>(() => {
    return searchParams.get('show_bots') || 'no'
  })
  // AI / path context, set by the analytics drill-downs (AI Agents card, AI
  // Agents detail page, Page detail). When present we scope the table to that
  // crawler / page and show a removable chip so the user understands why the
  // list is filtered.
  const [aiAgent, setAiAgent] = useState<string>(() => {
    return searchParams.get('ai_agent') || ''
  })
  const [aiProvider, setAiProvider] = useState<string>(() => {
    return searchParams.get('ai_provider') || ''
  })
  const [isAiAgent, setIsAiAgent] = useState<boolean>(() => {
    return searchParams.get('is_ai_agent') === 'true'
  })
  const [pathFilter, setPathFilter] = useState<string>(() => {
    return searchParams.get('path') || ''
  })
  // The advanced filters are collapsed by default — only the user-agent search
  // and the results summary stay visible until the user opens them.
  const [showFilters, setShowFilters] = useState<boolean>(() => {
    // Auto-open if the user arrived with a non-default filter in the URL.
    return (
      searchParams.get('filters') === 'open' ||
      !!searchParams.get('status_code') ||
      (!!searchParams.get('time_range') &&
        searchParams.get('time_range') !== '24h') ||
      (!!searchParams.get('environment') &&
        searchParams.get('environment') !== 'all') ||
      (!!searchParams.get('show_bots') &&
        searchParams.get('show_bots') !== 'no')
    )
  })

  const hasAiContext = !!aiAgent || !!aiProvider || isAiAgent

  // Calculate date range based on selected time range
  const dateRange = useMemo(() => {
    const now = new Date()
    let from: Date

    switch (timeRange) {
      case '1h':
        from = subHours(now, 1)
        break
      case '6h':
        from = subHours(now, 6)
        break
      case '24h':
        from = subHours(now, 24)
        break
      case '7d':
        from = subDays(now, 7)
        break
      case '30d':
        from = subDays(now, 30)
        break
      case '90d':
        from = subDays(now, 90)
        break
      default:
        from = subHours(now, 24)
    }

    return { from, to: now }
  }, [timeRange])

  const { data: environmentsData } = useQuery(
    getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    })
  )

  const { data: logs, isLoading } = useQuery(
    getProxyLogsOptions({
      query: {
        page: page,
        page_size: limit,
        project_id: project.id,
        status_code:
          statusCode && statusCode !== 'all' ? parseInt(statusCode) : undefined,
        method: method && method !== 'all' ? method : undefined,
        start_date: dateRange.from.toISOString(),
        end_date: dateRange.to.toISOString(),
        environment_id:
          environment && environment !== 'all'
            ? parseInt(environment)
            : undefined,
        // AI context implies bot traffic, so don't let the default "hide bots"
        // suppress the very rows the user drilled into. Otherwise honour the
        // explicit bot filter.
        is_bot: hasAiContext
          ? undefined
          : showBots === 'no'
            ? false
            : showBots === 'yes'
              ? true
              : undefined,
        is_ai_agent: isAiAgent ? true : undefined,
        ai_agent: aiAgent || undefined,
        ai_provider: aiProvider || undefined,
        path: pathFilter || undefined,
      },
    })
  )

  // Calculate total pages
  const totalPages = useMemo(() => {
    if (!logs) return 0
    return Math.ceil(logs.total / limit)
  }, [logs, limit])

  useEffect(() => {
    const newParams = new URLSearchParams()

    if (timeRange && timeRange !== '24h') {
      newParams.set('time_range', timeRange)
    }

    if (environment && environment !== 'all') {
      newParams.set('environment', environment)
    }

    if (statusCode && statusCode !== 'all') {
      newParams.set('status_code', statusCode)
    }

    if (showBots && showBots !== 'no') {
      newParams.set('show_bots', showBots)
    }

    if (aiAgent) {
      newParams.set('ai_agent', aiAgent)
    }
    if (aiProvider) {
      newParams.set('ai_provider', aiProvider)
    }
    if (isAiAgent) {
      newParams.set('is_ai_agent', 'true')
    }
    if (pathFilter) {
      newParams.set('path', pathFilter)
    }

    if (showFilters) {
      newParams.set('filters', 'open')
    }

    if (page > 1) {
      newParams.set('page', page.toString())
    }

    if (limit !== 25) {
      newParams.set('limit', limit.toString())
    }

    setSearchParams(newParams)
  }, [
    timeRange,
    environment,
    statusCode,
    showBots,
    aiAgent,
    aiProvider,
    isAiAgent,
    pathFilter,
    showFilters,
    page,
    limit,
    setSearchParams,
  ])

  const handleMethodChange = (value: string) => {
    setMethod(value)
    setPage(1)
  }

  const handleStatusCodeChange = (value: string) => {
    setStatusCode(value)
    setPage(1)
  }

  const handleEnvironmentChange = (value: string) => {
    setEnvironment(value)
    setPage(1)
  }

  const handleTimeRangeChange = (value: string) => {
    setTimeRange(value)
    setPage(1)
  }

  const handleLimitChange = (value: string) => {
    setLimit(parseInt(value))
    setPage(1)
  }

  const handleShowBotsChange = (value: string) => {
    setShowBots(value)
    setPage(1)
  }

  // Count of advanced filters currently narrowing the result set (everything
  // except the always-visible user-agent search and the default time range).
  const activeFilterCount = useMemo(() => {
    let n = 0
    if (timeRange !== '24h') n += 1
    if (method !== 'all') n += 1
    if (statusCode !== 'all') n += 1
    if (environment !== 'all') n += 1
    if (showBots !== 'no') n += 1
    return n
  }, [timeRange, method, statusCode, environment, showBots])

  const clearFilters = () => {
    setTimeRange('24h')
    setMethod('all')
    setStatusCode('all')
    setEnvironment('all')
    setShowBots('no')
    setPage(1)
  }

  const handleRowClick = (
    logId: number,
    logProjectId: number,
    timestamp: string
  ) => {
    if (onRowClick) {
      onRowClick(logId, logProjectId, timestamp)
    }
  }

  // Helper function to generate pagination button numbers
  const getPaginationPages = (currentPage: number, totalPages: number) => {
    const pageNumbers = []
    const maxButtons = 5
    let startPage = Math.max(1, currentPage - Math.floor(maxButtons / 2))
    const endPage = Math.min(totalPages, startPage + maxButtons - 1)

    if (endPage - startPage < maxButtons - 1) {
      startPage = Math.max(1, endPage - maxButtons + 1)
    }

    for (let i = startPage; i <= endPage; i++) {
      pageNumbers.push(i)
    }

    return pageNumbers
  }

  return (
    <div className="space-y-4 px-4 sm:px-0">
      {/* Header: title + results summary */}
      <div className="flex flex-col gap-1">
        <h2 className="text-lg font-semibold tracking-tight">Proxy Logs</h2>
        <p className="text-sm text-muted-foreground">
          {logs
            ? `${logs.total.toLocaleString()} request${logs.total === 1 ? '' : 's'} found`
            : 'Browse and analyze request logs'}
        </p>
      </div>

      {/* Toolbar: collapsed filters toggle + page size */}
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant={showFilters ? 'secondary' : 'outline'}
          size="sm"
          onClick={() => setShowFilters((v) => !v)}
        >
          <SlidersHorizontal className="mr-2 size-4" />
          Filters
          {activeFilterCount > 0 && (
            <Badge variant="secondary" className="ml-2 tabular-nums">
              {activeFilterCount}
            </Badge>
          )}
        </Button>
        <div className="ml-auto">
          <Select value={limit.toString()} onValueChange={handleLimitChange}>
            <SelectTrigger className="w-[110px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="10">10 / page</SelectItem>
              <SelectItem value="25">25 / page</SelectItem>
              <SelectItem value="50">50 / page</SelectItem>
              <SelectItem value="100">100 / page</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      {/* Collapsible advanced filters — hidden by default */}
      {showFilters && (
        <div className="rounded-lg border bg-muted/30 p-3 sm:p-4">
          <div className="flex flex-wrap items-end gap-2 sm:gap-3">
            <div className="flex flex-col gap-1">
              <label
                htmlFor="filter-time-range"
                className="text-xs font-medium text-muted-foreground"
              >
                Time range
              </label>
              <Select value={timeRange} onValueChange={handleTimeRangeChange}>
                <SelectTrigger id="filter-time-range" className="w-full sm:w-[160px]">
                  <SelectValue placeholder="Time range" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="1h">Last 1 hour</SelectItem>
                  <SelectItem value="6h">Last 6 hours</SelectItem>
                  <SelectItem value="24h">Last 24 hours</SelectItem>
                  <SelectItem value="7d">Last 7 days</SelectItem>
                  <SelectItem value="30d">Last 30 days</SelectItem>
                  <SelectItem value="90d">Last 90 days</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="flex flex-col gap-1">
              <label
                htmlFor="filter-method"
                className="text-xs font-medium text-muted-foreground"
              >
                Method
              </label>
              <Select value={method} onValueChange={handleMethodChange}>
                <SelectTrigger id="filter-method" className="w-full sm:w-[140px]">
                  <SelectValue placeholder="HTTP method" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All methods</SelectItem>
                  <SelectItem value="GET">GET</SelectItem>
                  <SelectItem value="POST">POST</SelectItem>
                  <SelectItem value="PUT">PUT</SelectItem>
                  <SelectItem value="DELETE">DELETE</SelectItem>
                  <SelectItem value="PATCH">PATCH</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="flex flex-col gap-1">
              <label
                htmlFor="filter-status"
                className="text-xs font-medium text-muted-foreground"
              >
                Status
              </label>
              <Select value={statusCode} onValueChange={handleStatusCodeChange}>
                <SelectTrigger id="filter-status" className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Status code" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All status codes</SelectItem>
                  <SelectGroup>
                    <SelectLabel>Success (2xx)</SelectLabel>
                    <SelectItem value="200">200 OK</SelectItem>
                    <SelectItem value="201">201 Created</SelectItem>
                    <SelectItem value="202">202 Accepted</SelectItem>
                    <SelectItem value="204">204 No Content</SelectItem>
                  </SelectGroup>
                  <SelectGroup>
                    <SelectLabel>Redirection (3xx)</SelectLabel>
                    <SelectItem value="301">301 Moved Permanently</SelectItem>
                    <SelectItem value="302">302 Found</SelectItem>
                    <SelectItem value="304">304 Not Modified</SelectItem>
                    <SelectItem value="307">307 Temporary Redirect</SelectItem>
                    <SelectItem value="308">308 Permanent Redirect</SelectItem>
                  </SelectGroup>
                  <SelectGroup>
                    <SelectLabel>Client Error (4xx)</SelectLabel>
                    <SelectItem value="400">400 Bad Request</SelectItem>
                    <SelectItem value="401">401 Unauthorized</SelectItem>
                    <SelectItem value="403">403 Forbidden</SelectItem>
                    <SelectItem value="404">404 Not Found</SelectItem>
                    <SelectItem value="405">405 Method Not Allowed</SelectItem>
                    <SelectItem value="409">409 Conflict</SelectItem>
                    <SelectItem value="422">422 Unprocessable Entity</SelectItem>
                    <SelectItem value="429">429 Too Many Requests</SelectItem>
                  </SelectGroup>
                  <SelectGroup>
                    <SelectLabel>Server Error (5xx)</SelectLabel>
                    <SelectItem value="500">500 Internal Server Error</SelectItem>
                    <SelectItem value="502">502 Bad Gateway</SelectItem>
                    <SelectItem value="503">503 Service Unavailable</SelectItem>
                    <SelectItem value="504">504 Gateway Timeout</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </div>

            {showEnvironmentFilter && (
              <div className="flex flex-col gap-1">
                <label
                  htmlFor="filter-environment"
                  className="text-xs font-medium text-muted-foreground"
                >
                  Environment
                </label>
                {isLoading ? (
                  <Skeleton className="h-9 w-full sm:w-[180px]" />
                ) : (
                  <Select
                    value={environment}
                    onValueChange={handleEnvironmentChange}
                  >
                    <SelectTrigger
                      id="filter-environment"
                      className="w-full sm:w-[180px]"
                    >
                      <SelectValue placeholder="Environment" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="all">All environments</SelectItem>
                      {environmentsData?.map((env) => (
                        <SelectItem key={env.id} value={env.id.toString()}>
                          {env.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              </div>
            )}

            <div className="flex flex-col gap-1">
              <label
                htmlFor="filter-bots"
                className="text-xs font-medium text-muted-foreground"
              >
                Bots
              </label>
              <Select value={showBots} onValueChange={handleShowBotsChange}>
                <SelectTrigger id="filter-bots" className="w-full sm:w-[150px]">
                  <SelectValue placeholder="Bot filter" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="no">Hide bots</SelectItem>
                  <SelectItem value="all">All traffic</SelectItem>
                  <SelectItem value="yes">Only bots</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {activeFilterCount > 0 && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={clearFilters}
                className="text-muted-foreground"
              >
                <X className="mr-1.5 size-4" />
                Clear filters
              </Button>
            )}
          </div>
        </div>
      )}

      {/* Context chips — show the AI agent / provider / page the table was
          drilled into, each removable. Without these the filter applies
          silently and the user can't tell why the list is scoped. */}
      {(hasAiContext || pathFilter) && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs text-muted-foreground">Filtered to:</span>
          {aiAgent && (
            <ContextChip
              icon={<AiAgentLogo agent={aiAgent} size={14} />}
              label={aiAgent}
              onRemove={() => {
                setAiAgent('')
                setIsAiAgent(false)
                setPage(1)
              }}
            />
          )}
          {aiProvider && (
            <ContextChip
              icon={<AiAgentLogo provider={aiProvider} size={14} />}
              label={aiProvider}
              onRemove={() => {
                setAiProvider('')
                setIsAiAgent(false)
                setPage(1)
              }}
            />
          )}
          {!aiAgent && !aiProvider && isAiAgent && (
            <ContextChip
              label="All AI agents"
              onRemove={() => {
                setIsAiAgent(false)
                setPage(1)
              }}
            />
          )}
          {pathFilter && (
            <ContextChip
              label={pathFilter}
              mono
              onRemove={() => {
                setPathFilter('')
                setPage(1)
              }}
            />
          )}
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-6 px-2 text-xs text-muted-foreground"
            onClick={() => {
              setAiAgent('')
              setAiProvider('')
              setIsAiAgent(false)
              setPathFilter('')
              setPage(1)
            }}
          >
            Clear all
          </Button>
        </div>
      )}

      <div className="space-y-4">
          {!isLoading && logs?.logs.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-16 text-center">
              <FileSearch className="h-16 w-16 text-muted-foreground mb-4" />
              <h3 className="text-lg font-semibold mb-2">
                No request logs found
              </h3>
              <p className="text-sm text-muted-foreground max-w-md">
                {statusCode !== 'all' ||
                method !== 'all' ||
                environment !== 'all'
                  ? 'Try adjusting your filters to see more results.'
                  : 'Start making requests to see logs appear here.'}
              </p>
            </div>
          ) : (
            <div className="-mx-4 overflow-x-auto whitespace-nowrap sm:mx-0">
              <div className="inline-block min-w-full px-4 align-middle sm:px-0">
                <Table className="w-full">
                  <TableHeader>
                    <TableRow>
                      <TableHead className="whitespace-nowrap">
                        Timestamp
                      </TableHead>
                      <TableHead className="whitespace-nowrap">Method</TableHead>
                      <TableHead className="whitespace-nowrap">URL</TableHead>
                      <TableHead className="whitespace-nowrap">Status</TableHead>
                      <TableHead className="whitespace-nowrap">
                        Duration
                      </TableHead>
                      <TableHead className="whitespace-nowrap">
                        User agent
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {isLoading
                      ? [...Array(5)].map((_, i) => (
                          <TableRow key={i}>
                            <TableCell>
                              <Skeleton className="h-6 w-32" />
                            </TableCell>
                            <TableCell>
                              <Skeleton className="h-6 w-16" />
                            </TableCell>
                            <TableCell>
                              <Skeleton className="h-6 w-96" />
                            </TableCell>
                            <TableCell>
                              <Skeleton className="h-6 w-16" />
                            </TableCell>
                            <TableCell>
                              <Skeleton className="h-6 w-20" />
                            </TableCell>
                            <TableCell>
                              <Skeleton className="h-6 w-40" />
                            </TableCell>
                          </TableRow>
                        ))
                      : logs?.logs.map((log) => (
                          <TableRow
                            key={log.id}
                            className={
                              onRowClick
                                ? 'cursor-pointer hover:bg-muted/50'
                                : ''
                            }
                            onClick={() =>
                              onRowClick &&
                              log.project_id &&
                              handleRowClick(
                                log.id,
                                log.project_id,
                                log.timestamp
                              )
                            }
                          >
                            <TableCell className="tabular-nums text-muted-foreground">
                              {format(
                                new Date(log.timestamp),
                                'yyyy-MM-dd HH:mm:ss'
                              )}
                            </TableCell>
                            <TableCell>
                              <span
                                className={`rounded-full px-2 py-1 text-xs font-medium ${
                                  log.method === 'GET'
                                    ? 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200'
                                    : log.method === 'POST'
                                      ? 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
                                      : log.method === 'DELETE'
                                        ? 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
                                        : log.method === 'PUT'
                                          ? 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200'
                                          : 'bg-gray-100 text-gray-800 dark:bg-gray-700 dark:text-gray-200'
                                }`}
                              >
                                {log.method}
                              </span>
                            </TableCell>
                            <TableCell className="max-w-[320px] truncate font-mono text-sm">
                              <span title={`https://${log.host}${log.path}`}>
                                https://{log.host}
                                {log.path}
                              </span>
                            </TableCell>
                            <TableCell>
                              <span
                                className={`rounded-full px-2 py-1 text-xs font-medium ${
                                  log.status_code >= 200 &&
                                  log.status_code < 300
                                    ? 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
                                    : log.status_code >= 400
                                      ? 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
                                      : 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200'
                                }`}
                              >
                                {log.status_code}
                              </span>
                            </TableCell>
                            <TableCell className="tabular-nums text-muted-foreground">
                              {log.response_time_ms
                                ? `${log.response_time_ms}ms`
                                : '-'}
                            </TableCell>
                            <TableCell className="max-w-[280px]">
                              {log.bot_name ? (
                                <span className="inline-flex items-center gap-1.5 truncate text-xs font-medium">
                                  <AiAgentLogo agent={log.bot_name} size={14} />
                                  {log.bot_name}
                                </span>
                              ) : (
                                <span
                                  className="block truncate font-mono text-xs text-muted-foreground"
                                  title={log.user_agent || ''}
                                >
                                  {log.user_agent || '-'}
                                </span>
                              )}
                            </TableCell>
                          </TableRow>
                        ))}
                  </TableBody>
                </Table>
              </div>
            </div>
          )}

          {/* Pagination */}
          {totalPages > 1 && (
            <div className="flex flex-col sm:flex-row items-center justify-between gap-4 mt-6">
              <div className="text-xs sm:text-sm text-muted-foreground text-center sm:text-left">
                Showing {(page - 1) * limit + 1} to{' '}
                {Math.min(page * limit, logs?.total || 0)} of {logs?.total || 0}{' '}
                logs
              </div>
              <div className="flex items-center gap-1 sm:gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => Math.max(1, p - 1))}
                  disabled={page === 1}
                  className="h-8 px-2 sm:h-9 sm:px-3"
                >
                  <ChevronLeft className="h-4 w-4" />
                  <span className="hidden sm:inline ml-1">Previous</span>
                </Button>
                {/* Desktop only: Show numbered page buttons */}
                <div className="hidden sm:flex items-center gap-1">
                  {getPaginationPages(page, totalPages).map((pageNum) => (
                    <Button
                      key={pageNum}
                      variant={pageNum === page ? 'default' : 'outline'}
                      size="sm"
                      onClick={() => setPage(pageNum)}
                      className="w-10"
                    >
                      {pageNum}
                    </Button>
                  ))}
                </div>
                {/* Mobile only: Show current page info */}
                <span className="sm:hidden text-xs text-muted-foreground px-2">
                  {page} / {totalPages}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                  disabled={page === totalPages}
                  className="h-8 px-2 sm:h-9 sm:px-3"
                >
                  <span className="hidden sm:inline mr-1">Next</span>
                  <ChevronRight className="h-4 w-4" />
                </Button>
              </div>
            </div>
          )}
      </div>
    </div>
  )
}

interface ContextChipProps {
  label: string
  icon?: React.ReactNode
  mono?: boolean
  onRemove: () => void
}

function ContextChip({ label, icon, mono, onRemove }: ContextChipProps) {
  return (
    <span className="inline-flex max-w-[240px] items-center gap-1.5 rounded-full border bg-muted/40 py-1 pr-1 pl-2 text-xs">
      {icon}
      <span className={`truncate ${mono ? 'font-mono' : 'font-medium'}`}>
        {label}
      </span>
      <button
        type="button"
        aria-label={`Remove ${label} filter`}
        onClick={onRemove}
        className="rounded-full p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
      >
        <X className="size-3.5" />
      </button>
    </span>
  )
}
