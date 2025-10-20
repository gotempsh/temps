import { getProxyLogsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProxyLogResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Filter,
  Loader2,
  Search,
} from 'lucide-react'
import { useState } from 'react'

interface ProxyLogsListProps {
  projectId?: number
  environmentId?: number
  onRowClick?: (logId: number) => void
}

export function ProxyLogsList({
  projectId,
  environmentId,
  onRowClick,
}: ProxyLogsListProps) {
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [routingStatus, setRoutingStatus] = useState<string | null>(null)
  const [statusCode, setStatusCode] = useState<string>('')
  const [requestSource, setRequestSource] = useState<string | null>(null)

  const { data, isLoading, error } = useQuery({
    ...getProxyLogsOptions({
      query: {
        project_id: projectId || null,
        environment_id: environmentId || null,
        routing_status: routingStatus,
        status_code: statusCode ? parseInt(statusCode) : null,
        request_source: requestSource,
        page,
        page_size: pageSize,
      },
    }),
    staleTime: 1000 * 30, // 30 seconds
  })

  const getStatusBadgeVariant = (statusCode: number) => {
    if (statusCode >= 200 && statusCode < 300) return 'default'
    if (statusCode >= 300 && statusCode < 400) return 'secondary'
    if (statusCode >= 400 && statusCode < 500) return 'destructive'
    if (statusCode >= 500) return 'destructive'
    return 'outline'
  }

  const getRoutingStatusBadge = (status: string) => {
    switch (status) {
      case 'routed':
        return <Badge variant="default">Routed</Badge>
      case 'failed':
        return <Badge variant="destructive">Failed</Badge>
      case 'not_found':
        return <Badge variant="secondary">Not Found</Badge>
      default:
        return <Badge variant="outline">{status}</Badge>
    }
  }

  const handlePageChange = (newPage: number) => {
    setPage(newPage)
  }

  return (
    <div className="space-y-4">
      {/* Filters */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Filter className="h-5 w-5" />
            Filters
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
            {/* Routing Status Filter */}
            <div>
              <label className="text-sm font-medium mb-2 block">
                Routing Status
              </label>
              <Select
                value={routingStatus || 'all'}
                onValueChange={(value) => {
                  setRoutingStatus(value === 'all' ? null : value)
                  setPage(1)
                }}
              >
                <SelectTrigger>
                  <SelectValue placeholder="All statuses" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All</SelectItem>
                  <SelectItem value="routed">Routed</SelectItem>
                  <SelectItem value="failed">Failed</SelectItem>
                  <SelectItem value="not_found">Not Found</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {/* Status Code Filter */}
            <div>
              <label className="text-sm font-medium mb-2 block">
                Status Code
              </label>
              <Input
                type="number"
                placeholder="e.g., 404"
                value={statusCode}
                onChange={(e) => {
                  setStatusCode(e.target.value)
                  setPage(1)
                }}
              />
            </div>

            {/* Request Source Filter */}
            <div>
              <label className="text-sm font-medium mb-2 block">
                Request Source
              </label>
              <Select
                value={requestSource || 'all'}
                onValueChange={(value) => {
                  setRequestSource(value === 'all' ? null : value)
                  setPage(1)
                }}
              >
                <SelectTrigger>
                  <SelectValue placeholder="All sources" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All</SelectItem>
                  <SelectItem value="proxy">Proxy</SelectItem>
                  <SelectItem value="api">API</SelectItem>
                  <SelectItem value="console">Console</SelectItem>
                  <SelectItem value="cli">CLI</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {/* Page Size */}
            <div>
              <label className="text-sm font-medium mb-2 block">
                Page Size
              </label>
              <Select
                value={pageSize.toString()}
                onValueChange={(value) => {
                  setPageSize(parseInt(value))
                  setPage(1)
                }}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="10">10</SelectItem>
                  <SelectItem value="20">20</SelectItem>
                  <SelectItem value="50">50</SelectItem>
                  <SelectItem value="100">100</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Results */}
      <Card>
        <CardContent className="p-0">
          {isLoading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <AlertCircle className="h-12 w-12 text-destructive mb-4" />
              <p className="text-lg font-semibold">Failed to load proxy logs</p>
              <p className="text-sm text-muted-foreground">
                Please try again later
              </p>
            </div>
          ) : !data || data.logs.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <Search className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-lg font-semibold">No proxy logs found</p>
              <p className="text-sm text-muted-foreground">
                Try adjusting your filters
              </p>
            </div>
          ) : (
            <>
              <div className="overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Timestamp</TableHead>
                      <TableHead>Method</TableHead>
                      <TableHead>Host</TableHead>
                      <TableHead>Path</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead>Routing</TableHead>
                      <TableHead>Source</TableHead>
                      <TableHead>IP</TableHead>
                      <TableHead>Response Time</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {data.logs.map((log: ProxyLogResponse) => (
                      <TableRow
                        key={log.id}
                        className={
                          onRowClick ? 'cursor-pointer hover:bg-muted/50' : ''
                        }
                        onClick={() => onRowClick?.(log.id)}
                      >
                        <TableCell className="font-mono text-xs">
                          {format(new Date(log.timestamp), 'MMM dd, HH:mm:ss')}
                        </TableCell>
                        <TableCell>
                          <Badge variant="outline">{log.method}</Badge>
                        </TableCell>
                        <TableCell className="font-mono text-xs max-w-[200px] truncate">
                          {log.host}
                        </TableCell>
                        <TableCell className="font-mono text-xs max-w-[300px] truncate">
                          {log.path}
                          {log.query_string && (
                            <span className="text-muted-foreground">
                              ?{log.query_string}
                            </span>
                          )}
                        </TableCell>
                        <TableCell>
                          <Badge
                            variant={getStatusBadgeVariant(log.status_code)}
                          >
                            {log.status_code}
                          </Badge>
                        </TableCell>
                        <TableCell>
                          {getRoutingStatusBadge(log.routing_status)}
                        </TableCell>
                        <TableCell>
                          <Badge variant="secondary" className="capitalize">
                            {log.request_source || 'proxy'}
                          </Badge>
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {log.client_ip || '-'}
                        </TableCell>
                        <TableCell className="text-xs">
                          {log.response_time_ms
                            ? `${log.response_time_ms}ms`
                            : '-'}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>

              {/* Pagination */}
              <div className="flex items-center justify-between px-4 py-4 border-t">
                <div className="text-sm text-muted-foreground">
                  Showing {(page - 1) * pageSize + 1} to{' '}
                  {Math.min(page * pageSize, data.total)} of {data.total}{' '}
                  results
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => handlePageChange(1)}
                    disabled={page === 1}
                  >
                    <ChevronsLeft className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => handlePageChange(page - 1)}
                    disabled={page === 1}
                  >
                    <ChevronLeft className="h-4 w-4" />
                  </Button>
                  <div className="flex items-center gap-2 px-2">
                    <span className="text-sm">
                      Page {page} of {data.total_pages}
                    </span>
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => handlePageChange(page + 1)}
                    disabled={page === data.total_pages}
                  >
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => handlePageChange(data.total_pages)}
                    disabled={page === data.total_pages}
                  >
                    <ChevronsRight className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
