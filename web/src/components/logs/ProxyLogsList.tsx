import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getProxyLogsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { ChevronLeft, ChevronRight, FileSearch } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

interface ProxyLogsListProps {
  project: ProjectResponse
  onRowClick?: (logId: number, projectId: number) => void
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
    return searchParams.get('time_range') || '24h'
  })
  const [environment, setEnvironment] = useState<string>(() => {
    return searchParams.get('environment') || 'all'
  })
  const [showBots, setShowBots] = useState<string>(() => {
    return searchParams.get('show_bots') || 'no'
  })

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
        is_bot:
          showBots === 'no' ? false : showBots === 'yes' ? true : undefined,
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

  const handleRowClick = (logId: number, logProjectId: number) => {
    if (onRowClick) {
      onRowClick(logId, logProjectId)
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
      <Card className="p-4">
        <CardHeader className="px-0">
          <div className="flex items-center justify-between">
            <div>
              <CardTitle>Proxy Logs</CardTitle>
              <CardDescription>
                {logs
                  ? `${logs.total.toLocaleString()} logs found`
                  : 'Browse and analyze request logs'}
              </CardDescription>
            </div>
            <Select value={limit.toString()} onValueChange={handleLimitChange}>
              <SelectTrigger className="w-[120px]">
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
        </CardHeader>
        <div className="space-y-4">
          <div className="flex gap-2 sm:gap-4 flex-wrap">
            <Select value={timeRange} onValueChange={handleTimeRangeChange}>
              <SelectTrigger className="w-full sm:w-[180px]">
                <SelectValue placeholder="Time Range" />
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
            <Select value={method} onValueChange={handleMethodChange}>
              <SelectTrigger className="w-full sm:w-[180px]">
                <SelectValue placeholder="HTTP Method" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All Methods</SelectItem>
                <SelectItem value="GET">GET</SelectItem>
                <SelectItem value="POST">POST</SelectItem>
                <SelectItem value="PUT">PUT</SelectItem>
                <SelectItem value="DELETE">DELETE</SelectItem>
                <SelectItem value="PATCH">PATCH</SelectItem>
              </SelectContent>
            </Select>
            <Select value={statusCode} onValueChange={handleStatusCodeChange}>
              <SelectTrigger className="w-full sm:w-[200px]">
                <SelectValue placeholder="Status Code" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All Status Codes</SelectItem>
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
            {showEnvironmentFilter &&
              (isLoading ? (
                <Skeleton className="h-10 w-full sm:w-[200px]" />
              ) : (
                <Select
                  value={environment}
                  onValueChange={handleEnvironmentChange}
                >
                  <SelectTrigger className="w-full sm:w-[200px]">
                    <SelectValue placeholder="Environment" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All Environments</SelectItem>
                    {environmentsData?.map((env) => (
                      <SelectItem key={env.id} value={env.id.toString()}>
                        {env.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              ))}
            <Select value={showBots} onValueChange={handleShowBotsChange}>
              <SelectTrigger className="w-full sm:w-[180px]">
                <SelectValue placeholder="Bot Filter" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="no">Hide Bots</SelectItem>
                <SelectItem value="all">All Traffic</SelectItem>
                <SelectItem value="yes">Only Bots</SelectItem>
              </SelectContent>
            </Select>
          </div>

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
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Timestamp</TableHead>
                  <TableHead>Method</TableHead>
                  <TableHead>URL</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Duration</TableHead>
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
                      </TableRow>
                    ))
                  : logs?.logs.map((log) => (
                      <TableRow
                        key={log.id}
                        className={
                          onRowClick ? 'cursor-pointer hover:bg-muted/50' : ''
                        }
                        onClick={() =>
                          onRowClick &&
                          log.project_id &&
                          handleRowClick(log.id, log.project_id)
                        }
                      >
                        <TableCell>
                          {format(
                            new Date(log.timestamp),
                            'yyyy-MM-dd HH:mm:ss'
                          )}
                        </TableCell>
                        <TableCell>
                          <span
                            className={`px-2 py-1 rounded-full text-xs font-medium
                        ${
                          log.method === 'GET'
                            ? 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200'
                            : log.method === 'POST'
                              ? 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
                              : log.method === 'DELETE'
                                ? 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
                                : log.method === 'PUT'
                                  ? 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200'
                                  : 'bg-gray-100 text-gray-800 dark:bg-gray-700 dark:text-gray-200'
                        }
                                          `}
                          >
                            {log.method}
                          </span>
                        </TableCell>
                        <TableCell className="font-mono text-sm">
                          https://{log.host}
                          {log.path}
                        </TableCell>
                        <TableCell>
                          <span
                            className={`px-2 py-1 rounded-full text-xs font-medium
                        ${
                          log.status_code >= 200 && log.status_code < 300
                            ? 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
                            : log.status_code >= 400
                              ? 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
                              : 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200'
                        }
                                          `}
                          >
                            {log.status_code}
                          </span>
                        </TableCell>
                        <TableCell>
                          {log.response_time_ms
                            ? `${log.response_time_ms}ms`
                            : '-'}
                        </TableCell>
                      </TableRow>
                    ))}
              </TableBody>
            </Table>
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
      </Card>
    </div>
  )
}
