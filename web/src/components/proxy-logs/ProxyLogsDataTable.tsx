import { getProxyLogsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProxyLogResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
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
  ArrowDown,
  ArrowUp,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Columns,
  Filter,
  Loader2,
  Search,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useSearchParams } from 'react-router-dom'

interface ProxyLogsDataTableProps {
  projectId?: number
  environmentId?: number
  onRowClick?: (log: ProxyLogResponse) => void
}

interface FilterState {
  deployment_id?: string
  start_date?: string
  end_date?: string
  method?: string
  host?: string
  path?: string
  client_ip?: string
  status_code?: string
  response_time_min?: string
  response_time_max?: string
  routing_status?: string
  request_source?: string
  is_system_request?: boolean | null
  user_agent?: string
  browser?: string
  operating_system?: string
  device_type?: string
  is_bot?: boolean | null
  bot_name?: string
  request_size_min?: string
  request_size_max?: string
  response_size_min?: string
  response_size_max?: string
  cache_status?: string
  container_id?: string
  upstream_host?: string
  has_error?: boolean | null
}

type ColumnKey =
  | 'timestamp'
  | 'method'
  | 'host'
  | 'path'
  | 'status_code'
  | 'routing_status'
  | 'request_source'
  | 'client_ip'
  | 'response_time_ms'
  | 'device_type'
  | 'browser'
  | 'is_bot'
  | 'bot_name'
  | 'cache_status'
  | 'upstream_host'

const STORAGE_KEY = 'proxy-logs-visible-columns'

const getInitialVisibleColumns = (): Set<ColumnKey> => {
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored) {
      const parsed = JSON.parse(stored) as ColumnKey[]
      return new Set(parsed)
    }
  } catch (error) {
    console.error('Failed to parse stored columns:', error)
  }
  // Default columns
  return new Set([
    'timestamp',
    'method',
    'path',
    'status_code',
    'routing_status',
    'response_time_ms',
  ])
}

export function ProxyLogsDataTable({
  projectId,
  environmentId,
  onRowClick,
}: ProxyLogsDataTableProps) {
  const [searchParams, setSearchParams] = useSearchParams()

  // Initialize page from URL params or default to 1
  const [page, setPage] = useState(() => {
    const pageParam = searchParams.get('page')
    return pageParam ? parseInt(pageParam, 10) : 1
  })

  const [pageSize, setPageSize] = useState(20)
  const [sortBy, setSortBy] = useState<string>('timestamp')
  const [sortOrder, setSortOrder] = useState<'asc' | 'desc'>('desc')
  const [showFilters, setShowFilters] = useState(false)
  const [filters, setFilters] = useState<FilterState>({})
  const [pendingFilters, setPendingFilters] = useState<FilterState>({})
  const [visibleColumns, setVisibleColumns] = useState<Set<ColumnKey>>(
    getInitialVisibleColumns()
  )

  // Update URL when page changes
  useEffect(() => {
    const newParams = new URLSearchParams(searchParams)
    newParams.set('page', page.toString())
    setSearchParams(newParams, { replace: true })
  }, [page, searchParams, setSearchParams])

  const { data, isLoading, error } = useQuery({
    ...getProxyLogsOptions({
      query: {
        project_id: projectId || null,
        environment_id: environmentId || null,
        deployment_id: filters.deployment_id
          ? parseInt(filters.deployment_id)
          : null,
        start_date: filters.start_date || null,
        end_date: filters.end_date || null,
        method: filters.method || null,
        host: filters.host || null,
        path: filters.path || null,
        client_ip: filters.client_ip || null,
        status_code: filters.status_code ? parseInt(filters.status_code) : null,
        response_time_min: filters.response_time_min
          ? parseInt(filters.response_time_min)
          : null,
        response_time_max: filters.response_time_max
          ? parseInt(filters.response_time_max)
          : null,
        routing_status: filters.routing_status || null,
        request_source: filters.request_source || null,
        is_system_request: filters.is_system_request,
        user_agent: filters.user_agent || null,
        browser: filters.browser || null,
        operating_system: filters.operating_system || null,
        device_type: filters.device_type || null,
        is_bot: filters.is_bot,
        bot_name: filters.bot_name || null,
        request_size_min: filters.request_size_min
          ? parseInt(filters.request_size_min)
          : null,
        request_size_max: filters.request_size_max
          ? parseInt(filters.request_size_max)
          : null,
        response_size_min: filters.response_size_min
          ? parseInt(filters.response_size_min)
          : null,
        response_size_max: filters.response_size_max
          ? parseInt(filters.response_size_max)
          : null,
        cache_status: filters.cache_status || null,
        container_id: filters.container_id || null,
        upstream_host: filters.upstream_host || null,
        has_error: filters.has_error,
        page,
        page_size: pageSize,
        sort_by: sortBy,
        sort_order: sortOrder,
      },
    }),
    staleTime: 1000 * 30, // 30 seconds
  })

  const handleSort = (column: string) => {
    if (sortBy === column) {
      setSortOrder(sortOrder === 'asc' ? 'desc' : 'asc')
    } else {
      setSortBy(column)
      setSortOrder('desc')
    }
    setPage(1)
  }

  const toggleColumn = (column: ColumnKey) => {
    const newColumns = new Set(visibleColumns)
    if (newColumns.has(column)) {
      newColumns.delete(column)
    } else {
      newColumns.add(column)
    }
    setVisibleColumns(newColumns)
  }

  // Save visible columns to localStorage whenever they change
  useEffect(() => {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify(Array.from(visibleColumns))
      )
    } catch (error) {
      console.error('Failed to save visible columns:', error)
    }
  }, [visibleColumns])

  const applyFilters = () => {
    setFilters(pendingFilters)
    setPage(1)
  }

  const clearFilters = () => {
    setFilters({})
    setPendingFilters({})
    setPage(1)
  }

  const handleFilterKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      applyFilters()
    }
  }

  const hasActiveFilters = Object.keys(filters).some(
    (key) =>
      filters[key as keyof FilterState] !== undefined &&
      filters[key as keyof FilterState] !== null
  )

  const hasPendingChanges =
    JSON.stringify(filters) !== JSON.stringify(pendingFilters)

  const getStatusBadgeVariant = (statusCode: number) => {
    if (statusCode >= 200 && statusCode < 300) return 'default'
    if (statusCode >= 300 && statusCode < 400) return 'secondary'
    return 'destructive'
  }

  const getRoutingStatusBadge = (status: string) => {
    const variants: Record<string, string> = {
      routed: 'default',
      failed: 'destructive',
      not_found: 'secondary',
      no_project: 'secondary',
      error: 'destructive',
    }
    return (
      <Badge variant={(variants[status] as any) || 'outline'}>{status}</Badge>
    )
  }

  const columns: Array<{ key: ColumnKey; label: string; sortable?: boolean }> =
    [
      { key: 'timestamp', label: 'Timestamp', sortable: true },
      { key: 'method', label: 'Method', sortable: true },
      { key: 'host', label: 'Host', sortable: true },
      { key: 'path', label: 'Path', sortable: true },
      { key: 'status_code', label: 'Status', sortable: true },
      { key: 'routing_status', label: 'Routing', sortable: true },
      { key: 'request_source', label: 'Source', sortable: true },
      { key: 'client_ip', label: 'IP', sortable: true },
      { key: 'response_time_ms', label: 'Response Time', sortable: true },
      { key: 'device_type', label: 'Device', sortable: true },
      { key: 'browser', label: 'Browser', sortable: true },
      { key: 'is_bot', label: 'Bot', sortable: true },
      { key: 'bot_name', label: 'Bot Name', sortable: true },
      { key: 'cache_status', label: 'Cache', sortable: true },
      { key: 'upstream_host', label: 'Upstream', sortable: true },
    ]

  return (
    <div className="space-y-4">
      {/* Toolbar */}
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div className="flex items-center gap-2">
          <Button
            variant={showFilters ? 'default' : 'outline'}
            size="sm"
            onClick={() => setShowFilters(!showFilters)}
          >
            <Filter className="h-4 w-4 mr-2" />
            Filters
            {hasActiveFilters && (
              <Badge variant="secondary" className="ml-2">
                {
                  Object.keys(filters).filter(
                    (k) => filters[k as keyof FilterState]
                  ).length
                }
              </Badge>
            )}
          </Button>
          {hasActiveFilters && (
            <Button variant="ghost" size="sm" onClick={clearFilters}>
              <X className="h-4 w-4 mr-2" />
              Clear
            </Button>
          )}
        </div>
        <div className="flex items-center gap-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm">
                <Columns className="h-4 w-4 mr-2" />
                Columns
                <ChevronDown className="h-4 w-4 ml-2" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-48">
              {columns.map((column) => (
                <DropdownMenuCheckboxItem
                  key={column.key}
                  checked={visibleColumns.has(column.key)}
                  onCheckedChange={() => toggleColumn(column.key)}
                >
                  {column.label}
                </DropdownMenuCheckboxItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <Select
            value={pageSize.toString()}
            onValueChange={(v) => {
              setPageSize(parseInt(v))
              setPage(1)
            }}
          >
            <SelectTrigger className="w-[100px]">
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

      {/* Advanced Filters */}
      {showFilters && (
        <Card>
          <CardContent className="pt-6">
            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-4">
              {/* Date Range */}
              <div>
                <Label>Start Date</Label>
                <Input
                  type="datetime-local"
                  value={pendingFilters.start_date || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      start_date: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>
              <div>
                <Label>End Date</Label>
                <Input
                  type="datetime-local"
                  value={pendingFilters.end_date || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      end_date: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* HTTP Fields */}
              <div>
                <Label>Method</Label>
                <Select
                  value={pendingFilters.method || 'all'}
                  onValueChange={(v) =>
                    setPendingFilters({
                      ...pendingFilters,
                      method: v === 'all' ? undefined : v,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All</SelectItem>
                    <SelectItem value="GET">GET</SelectItem>
                    <SelectItem value="POST">POST</SelectItem>
                    <SelectItem value="PUT">PUT</SelectItem>
                    <SelectItem value="DELETE">DELETE</SelectItem>
                    <SelectItem value="PATCH">PATCH</SelectItem>
                    <SelectItem value="HEAD">HEAD</SelectItem>
                    <SelectItem value="OPTIONS">OPTIONS</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <Label>Host</Label>
                <Input
                  placeholder="example.com"
                  value={pendingFilters.host || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      host: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Path</Label>
                <Input
                  placeholder="/api/..."
                  value={pendingFilters.path || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      path: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Status Code</Label>
                <Input
                  type="number"
                  placeholder="200"
                  value={pendingFilters.status_code || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      status_code: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* Routing */}
              <div>
                <Label>Routing Status</Label>
                <Select
                  value={pendingFilters.routing_status || 'all'}
                  onValueChange={(v) =>
                    setPendingFilters({
                      ...pendingFilters,
                      routing_status: v === 'all' ? undefined : v,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All</SelectItem>
                    <SelectItem value="routed">Routed</SelectItem>
                    <SelectItem value="failed">Failed</SelectItem>
                    <SelectItem value="not_found">Not Found</SelectItem>
                    <SelectItem value="no_project">No Project</SelectItem>
                    <SelectItem value="error">Error</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <Label>Request Source</Label>
                <Select
                  value={pendingFilters.request_source || 'all'}
                  onValueChange={(v) =>
                    setPendingFilters({
                      ...pendingFilters,
                      request_source: v === 'all' ? undefined : v,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
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

              {/* Performance */}
              <div>
                <Label>Min Response Time (ms)</Label>
                <Input
                  type="number"
                  placeholder="0"
                  value={pendingFilters.response_time_min || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      response_time_min: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Max Response Time (ms)</Label>
                <Input
                  type="number"
                  placeholder="1000"
                  value={pendingFilters.response_time_max || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      response_time_max: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* Client Info */}
              <div>
                <Label>Client IP</Label>
                <Input
                  placeholder="192.168.1.1"
                  value={pendingFilters.client_ip || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      client_ip: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Device Type</Label>
                <Select
                  value={pendingFilters.device_type || 'all'}
                  onValueChange={(v) =>
                    setPendingFilters({
                      ...pendingFilters,
                      device_type: v === 'all' ? undefined : v,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All</SelectItem>
                    <SelectItem value="mobile">Mobile</SelectItem>
                    <SelectItem value="desktop">Desktop</SelectItem>
                    <SelectItem value="tablet">Tablet</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <Label>Browser</Label>
                <Input
                  placeholder="Chrome"
                  value={pendingFilters.browser || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      browser: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Operating System</Label>
                <Input
                  placeholder="Windows"
                  value={pendingFilters.operating_system || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      operating_system: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>User Agent</Label>
                <Input
                  placeholder="Mozilla/5.0..."
                  value={pendingFilters.user_agent || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      user_agent: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* Bot Detection */}
              <div>
                <Label>Bot Name</Label>
                <Input
                  placeholder="Googlebot"
                  value={pendingFilters.bot_name || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      bot_name: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* Infrastructure */}
              <div>
                <Label>Cache Status</Label>
                <Input
                  placeholder="HIT"
                  value={pendingFilters.cache_status || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      cache_status: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Container ID</Label>
                <Input
                  placeholder="abc123..."
                  value={pendingFilters.container_id || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      container_id: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Upstream Host</Label>
                <Input
                  placeholder="backend:8080"
                  value={pendingFilters.upstream_host || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      upstream_host: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Deployment ID</Label>
                <Input
                  type="number"
                  placeholder="123"
                  value={pendingFilters.deployment_id || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      deployment_id: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* Size Filters */}
              <div>
                <Label>Min Request Size (bytes)</Label>
                <Input
                  type="number"
                  placeholder="0"
                  value={pendingFilters.request_size_min || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      request_size_min: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Max Request Size (bytes)</Label>
                <Input
                  type="number"
                  placeholder="1000000"
                  value={pendingFilters.request_size_max || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      request_size_max: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Min Response Size (bytes)</Label>
                <Input
                  type="number"
                  placeholder="0"
                  value={pendingFilters.response_size_min || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      response_size_min: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              <div>
                <Label>Max Response Size (bytes)</Label>
                <Input
                  type="number"
                  placeholder="1000000"
                  value={pendingFilters.response_size_max || ''}
                  onChange={(e) =>
                    setPendingFilters({
                      ...pendingFilters,
                      response_size_max: e.target.value,
                    })
                  }
                  onKeyDown={handleFilterKeyDown}
                />
              </div>

              {/* Boolean Filters */}
              <div className="flex items-center space-x-2">
                <Checkbox
                  id="is-bot"
                  checked={pendingFilters.is_bot === true}
                  onCheckedChange={(checked) =>
                    setPendingFilters({
                      ...pendingFilters,
                      is_bot: checked ? true : null,
                    })
                  }
                />
                <Label htmlFor="is-bot">Is Bot</Label>
              </div>

              <div className="flex items-center space-x-2">
                <Checkbox
                  id="is-system"
                  checked={pendingFilters.is_system_request === true}
                  onCheckedChange={(checked) =>
                    setPendingFilters({
                      ...pendingFilters,
                      is_system_request: checked ? true : null,
                    })
                  }
                />
                <Label htmlFor="is-system">System Request</Label>
              </div>

              <div className="flex items-center space-x-2">
                <Checkbox
                  id="has-error"
                  checked={pendingFilters.has_error === true}
                  onCheckedChange={(checked) =>
                    setPendingFilters({
                      ...pendingFilters,
                      has_error: checked ? true : null,
                    })
                  }
                />
                <Label htmlFor="has-error">Has Error</Label>
              </div>
            </div>

            {/* Apply Filters Button */}
            <div className="flex items-center justify-between pt-4 border-t mt-4">
              <Button variant="outline" onClick={clearFilters}>
                Clear All Filters
              </Button>
              <Button onClick={applyFilters} disabled={!hasPendingChanges}>
                Apply Filters
                {hasPendingChanges && (
                  <Badge
                    variant="secondary"
                    className="ml-2 bg-orange-500 text-white"
                  >
                    Unsaved
                  </Badge>
                )}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Results Table */}
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
                      {columns
                        .filter((col) => visibleColumns.has(col.key))
                        .map((column) => (
                          <TableHead key={column.key}>
                            {column.sortable ? (
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => handleSort(column.key)}
                                className="-ml-3 h-8"
                              >
                                {column.label}
                                {sortBy === column.key &&
                                  (sortOrder === 'asc' ? (
                                    <ArrowUp className="ml-2 h-4 w-4" />
                                  ) : (
                                    <ArrowDown className="ml-2 h-4 w-4" />
                                  ))}
                              </Button>
                            ) : (
                              column.label
                            )}
                          </TableHead>
                        ))}
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {data.logs.map((log: ProxyLogResponse) => (
                      <TableRow
                        key={log.id}
                        className={
                          onRowClick ? 'cursor-pointer hover:bg-muted/50' : ''
                        }
                        onClick={() => onRowClick?.(log)}
                      >
                        {visibleColumns.has('timestamp') && (
                          <TableCell className="font-mono text-xs">
                            {format(
                              new Date(log.timestamp),
                              'MMM dd, HH:mm:ss'
                            )}
                          </TableCell>
                        )}
                        {visibleColumns.has('method') && (
                          <TableCell>
                            <Badge variant="outline">{log.method}</Badge>
                          </TableCell>
                        )}
                        {visibleColumns.has('host') && (
                          <TableCell className="font-mono text-xs max-w-[200px] truncate">
                            {log.host}
                          </TableCell>
                        )}
                        {visibleColumns.has('path') && (
                          <TableCell className="font-mono text-xs max-w-[300px] truncate">
                            {log.path}
                          </TableCell>
                        )}
                        {visibleColumns.has('status_code') && (
                          <TableCell>
                            <Badge
                              variant={getStatusBadgeVariant(log.status_code)}
                            >
                              {log.status_code}
                            </Badge>
                          </TableCell>
                        )}
                        {visibleColumns.has('routing_status') && (
                          <TableCell>
                            {getRoutingStatusBadge(log.routing_status)}
                          </TableCell>
                        )}
                        {visibleColumns.has('request_source') && (
                          <TableCell>
                            <Badge variant="secondary" className="capitalize">
                              {log.request_source}
                            </Badge>
                          </TableCell>
                        )}
                        {visibleColumns.has('client_ip') && (
                          <TableCell className="font-mono text-xs">
                            {log.client_ip || '-'}
                          </TableCell>
                        )}
                        {visibleColumns.has('response_time_ms') && (
                          <TableCell className="text-xs">
                            {log.response_time_ms
                              ? `${log.response_time_ms}ms`
                              : '-'}
                          </TableCell>
                        )}
                        {visibleColumns.has('device_type') && (
                          <TableCell className="capitalize text-xs">
                            {log.device_type || '-'}
                          </TableCell>
                        )}
                        {visibleColumns.has('browser') && (
                          <TableCell className="text-xs">
                            {log.browser || '-'}
                          </TableCell>
                        )}
                        {visibleColumns.has('is_bot') && (
                          <TableCell>
                            {log.is_bot && (
                              <Badge variant="secondary">Bot</Badge>
                            )}
                          </TableCell>
                        )}
                        {visibleColumns.has('bot_name') && (
                          <TableCell className="text-xs">
                            {log.bot_name || '-'}
                          </TableCell>
                        )}
                        {visibleColumns.has('cache_status') && (
                          <TableCell>
                            {log.cache_status && (
                              <Badge variant="outline">
                                {log.cache_status}
                              </Badge>
                            )}
                          </TableCell>
                        )}
                        {visibleColumns.has('upstream_host') && (
                          <TableCell className="font-mono text-xs max-w-[150px] truncate">
                            {log.upstream_host || '-'}
                          </TableCell>
                        )}
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
                    onClick={() => setPage(1)}
                    disabled={page === 1}
                  >
                    <ChevronsLeft className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setPage(page - 1)}
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
                    onClick={() => setPage(page + 1)}
                    disabled={page === data.total_pages}
                  >
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setPage(data.total_pages)}
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
