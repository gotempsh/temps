import { getProxyLogsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProxyLogResponse } from '@/api/client/types.gen'
import { AiAgentLogo } from '@/components/ui/ai-agent-logo'
import { AGENT_TO_PROVIDER, AI_PROVIDERS } from '@/lib/ai-agents'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
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
import { Separator } from '@/components/ui/separator'
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
  ChevronRight as ChevronExpand,
  ChevronsLeft,
  ChevronsRight,
  Columns,
  ExternalLink,
  Filter,
  Loader2,
  Search,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'

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
  ai_provider?: string
  ai_agent?: string
  is_ai_agent?: boolean | null
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

// Helper to parse FilterState from URL search params
function parseFiltersFromParams(params: URLSearchParams): FilterState {
  const f: FilterState = {}
  const str = (key: string) => params.get(key) || undefined
  const bool = (key: string) => {
    const v = params.get(key)
    if (v === 'true') return true
    if (v === 'false') return false
    return null
  }
  f.deployment_id = str('deployment_id')
  f.start_date = str('start_date')
  f.end_date = str('end_date')
  f.method = str('method')
  f.host = str('host')
  f.path = str('path')
  f.client_ip = str('client_ip')
  f.status_code = str('status_code')
  f.response_time_min = str('response_time_min')
  f.response_time_max = str('response_time_max')
  f.routing_status = str('routing_status')
  f.request_source = str('request_source')
  f.is_system_request = bool('is_system_request')
  f.user_agent = str('user_agent')
  f.browser = str('browser')
  f.operating_system = str('operating_system')
  f.device_type = str('device_type')
  f.is_bot = bool('is_bot')
  f.bot_name = str('bot_name')
  f.ai_provider = str('ai_provider')
  f.ai_agent = str('ai_agent')
  f.is_ai_agent = bool('is_ai_agent')
  f.request_size_min = str('request_size_min')
  f.request_size_max = str('request_size_max')
  f.response_size_min = str('response_size_min')
  f.response_size_max = str('response_size_max')
  f.cache_status = str('cache_status')
  f.container_id = str('container_id')
  f.upstream_host = str('upstream_host')
  f.has_error = bool('has_error')
  // Clean out undefined values
  return Object.fromEntries(
    Object.entries(f).filter(([, v]) => v !== undefined && v !== null)
  ) as FilterState
}

function serializeFiltersToParams(
  filters: FilterState,
  params: URLSearchParams
) {
  // Filter keys to serialize
  const filterKeys: (keyof FilterState)[] = [
    'deployment_id',
    'start_date',
    'end_date',
    'method',
    'host',
    'path',
    'client_ip',
    'status_code',
    'response_time_min',
    'response_time_max',
    'routing_status',
    'request_source',
    'is_system_request',
    'user_agent',
    'browser',
    'operating_system',
    'device_type',
    'is_bot',
    'bot_name',
    'ai_provider',
    'ai_agent',
    'is_ai_agent',
    'request_size_min',
    'request_size_max',
    'response_size_min',
    'response_size_max',
    'cache_status',
    'container_id',
    'upstream_host',
    'has_error',
  ]
  for (const key of filterKeys) {
    const val = filters[key]
    if (val !== undefined && val !== null && val !== '') {
      params.set(key, String(val))
    } else {
      params.delete(key)
    }
  }
}

function formatBytes(bytes: number | null | undefined): string {
  if (!bytes) return '-'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export function ProxyLogsDataTable({
  projectId,
  environmentId,
  onRowClick,
}: ProxyLogsDataTableProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const isInitialMount = useRef(true)

  // Initialize ALL state from URL search params
  const [page, setPage] = useState(() => {
    const v = searchParams.get('page')
    return v ? parseInt(v, 10) : 1
  })

  const [pageSize, setPageSize] = useState(() => {
    const v = searchParams.get('page_size')
    return v ? parseInt(v, 10) : 20
  })

  const [sortBy, setSortBy] = useState<string>(() => {
    return searchParams.get('sort_by') || 'timestamp'
  })

  const [sortOrder, setSortOrder] = useState<'asc' | 'desc'>(() => {
    const v = searchParams.get('sort_order')
    return v === 'asc' ? 'asc' : 'desc'
  })

  const [showFilters, setShowFilters] = useState(() => {
    return searchParams.get('filters') === 'open'
  })

  const [filters, setFilters] = useState<FilterState>(() =>
    parseFiltersFromParams(searchParams)
  )

  const [pendingFilters, setPendingFilters] = useState<FilterState>(() =>
    parseFiltersFromParams(searchParams)
  )

  const [visibleColumns, setVisibleColumns] = useState<Set<ColumnKey>>(
    getInitialVisibleColumns()
  )

  // Keyed by request_id, not serial id: the ClickHouse backend surfaces id=0
  // on every row, which would make id-keyed expansion toggle all rows at once.
  const [expandedRows, setExpandedRows] = useState<Set<string>>(new Set())

  // Sync ALL state to URL search params
  const syncToUrl = useCallback(() => {
    const params = new URLSearchParams()
    if (page !== 1) params.set('page', page.toString())
    if (pageSize !== 20) params.set('page_size', pageSize.toString())
    if (sortBy !== 'timestamp') params.set('sort_by', sortBy)
    if (sortOrder !== 'desc') params.set('sort_order', sortOrder)
    if (showFilters) params.set('filters', 'open')
    serializeFiltersToParams(filters, params)
    setSearchParams(params, { replace: true })
  }, [page, pageSize, sortBy, sortOrder, showFilters, filters, setSearchParams])

  useEffect(() => {
    // Skip the initial mount to avoid overwriting params we just read from
    if (isInitialMount.current) {
      isInitialMount.current = false
      return
    }
    syncToUrl()
  }, [syncToUrl])

  const toggleRow = useCallback((requestId: string) => {
    setExpandedRows((prev) => {
      const next = new Set(prev)
      if (next.has(requestId)) {
        next.delete(requestId)
      } else {
        next.add(requestId)
      }
      return next
    })
  }, [])

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
        ai_provider: filters.ai_provider || null,
        ai_agent: filters.ai_agent || null,
        is_ai_agent: filters.is_ai_agent,
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

  const aiAgentActive =
    filters.is_ai_agent === true ||
    !!filters.ai_provider ||
    !!filters.ai_agent

  return (
    <div className="space-y-4">
      {/* AI Agents quick-filter row. Clicking a provider pill sets
          ai_provider + flips is_ai_agent so the table re-fetches with the
          server-side `bot_name IN (agents_of_provider)` predicate. */}
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-xs text-muted-foreground mr-1">AI agents:</span>
        <Button
          variant={
            filters.is_ai_agent === true && !filters.ai_provider
              ? 'default'
              : 'outline'
          }
          size="sm"
          className="h-7 gap-1.5"
          onClick={() => {
            const next: FilterState = {
              ...filters,
              is_ai_agent: filters.is_ai_agent === true ? null : true,
              ai_provider: undefined,
              ai_agent: undefined,
            }
            setFilters(next)
            setPendingFilters(next)
            setPage(1)
          }}
        >
          <AiAgentLogo provider="OpenAI" size={14} className="opacity-60" />
          All AI traffic
        </Button>
        {AI_PROVIDERS.slice(0, 8).map((p) => {
          const active = filters.ai_provider === p.provider
          return (
            <Button
              key={p.provider}
              variant={active ? 'default' : 'outline'}
              size="sm"
              className="h-7 gap-1.5"
              onClick={() => {
                const next: FilterState = {
                  ...filters,
                  ai_provider: active ? undefined : p.provider,
                  ai_agent: undefined,
                  is_ai_agent: active ? null : true,
                }
                setFilters(next)
                setPendingFilters(next)
                setPage(1)
              }}
            >
              <AiAgentLogo provider={p.provider} size={14} />
              {p.provider}
            </Button>
          )
        })}
        {aiAgentActive && (
          <Button
            variant="ghost"
            size="sm"
            className="h-7 text-xs"
            onClick={() => {
              const next: FilterState = {
                ...filters,
                ai_provider: undefined,
                ai_agent: undefined,
                is_ai_agent: null,
              }
              setFilters(next)
              setPendingFilters(next)
              setPage(1)
            }}
          >
            <X className="h-3 w-3 mr-1" />
            Clear AI
          </Button>
        )}
      </div>

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

              <div>
                <Label>AI Provider</Label>
                <Select
                  value={pendingFilters.ai_provider || 'all'}
                  onValueChange={(v) =>
                    setPendingFilters({
                      ...pendingFilters,
                      ai_provider: v === 'all' ? undefined : v,
                      ai_agent: undefined,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All providers</SelectItem>
                    {AI_PROVIDERS.map((p) => (
                      <SelectItem key={p.provider} value={p.provider}>
                        {p.provider}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              <div>
                <Label>AI Agent</Label>
                <Select
                  value={pendingFilters.ai_agent || 'all'}
                  onValueChange={(v) =>
                    setPendingFilters({
                      ...pendingFilters,
                      ai_agent: v === 'all' ? undefined : v,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All agents</SelectItem>
                    {(pendingFilters.ai_provider
                      ? AI_PROVIDERS.find(
                          (p) => p.provider === pendingFilters.ai_provider
                        )?.agents ?? []
                      : AI_PROVIDERS.flatMap((p) => p.agents)
                    ).map((agent) => (
                      <SelectItem key={agent} value={agent}>
                        {agent}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
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
                  id="is-ai-agent"
                  checked={pendingFilters.is_ai_agent === true}
                  onCheckedChange={(checked) =>
                    setPendingFilters({
                      ...pendingFilters,
                      is_ai_agent: checked ? true : null,
                    })
                  }
                />
                <Label htmlFor="is-ai-agent">Is AI Agent</Label>
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
                      <TableHead className="w-8" />
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
                    {data.logs.map((log: ProxyLogResponse) => {
                      const isExpanded = expandedRows.has(log.request_id)
                      const visibleCount = columns.filter((col) =>
                        visibleColumns.has(col.key)
                      ).length

                      return (
                        <ProxyLogTableRow
                          key={log.request_id}
                          log={log}
                          isExpanded={isExpanded}
                          visibleColumns={visibleColumns}
                          visibleCount={visibleCount}
                          onToggle={() => {
                            if (onRowClick) {
                              onRowClick(log)
                            } else {
                              toggleRow(log.request_id)
                            }
                          }}
                          getStatusBadgeVariant={getStatusBadgeVariant}
                          getRoutingStatusBadge={getRoutingStatusBadge}
                        />
                      )
                    })}
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

// --- Expandable row component ---

interface ProxyLogTableRowProps {
  log: ProxyLogResponse
  isExpanded: boolean
  visibleColumns: Set<ColumnKey>
  visibleCount: number
  onToggle: () => void
  getStatusBadgeVariant: (code: number) => string
  getRoutingStatusBadge: (status: string) => React.ReactNode
}

function ProxyLogTableRow({
  log,
  isExpanded,
  visibleColumns,
  visibleCount,
  onToggle,
  getStatusBadgeVariant,
  getRoutingStatusBadge,
}: ProxyLogTableRowProps) {
  return (
    <>
      <TableRow
        className="cursor-pointer hover:bg-muted/50"
        onClick={onToggle}
      >
        <TableCell className="w-8 px-2">
          <ChevronExpand
            className={`h-4 w-4 text-muted-foreground transition-transform duration-200 ${
              isExpanded ? 'rotate-90' : ''
            }`}
          />
        </TableCell>
        {visibleColumns.has('timestamp') && (
          <TableCell className="font-mono text-xs">
            {format(new Date(log.timestamp), 'MMM dd, HH:mm:ss')}
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
            <Badge variant={getStatusBadgeVariant(log.status_code) as any}>
              {log.status_code}
            </Badge>
          </TableCell>
        )}
        {visibleColumns.has('routing_status') && (
          <TableCell>{getRoutingStatusBadge(log.routing_status)}</TableCell>
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
            {log.response_time_ms ? `${log.response_time_ms}ms` : '-'}
          </TableCell>
        )}
        {visibleColumns.has('device_type') && (
          <TableCell className="capitalize text-xs">
            {log.device_type || '-'}
          </TableCell>
        )}
        {visibleColumns.has('browser') && (
          <TableCell className="text-xs">{log.browser || '-'}</TableCell>
        )}
        {visibleColumns.has('is_bot') && (
          <TableCell>
            {log.is_bot && <Badge variant="secondary">Bot</Badge>}
          </TableCell>
        )}
        {visibleColumns.has('bot_name') && (
          <TableCell className="text-xs">
            {log.bot_name ? (
              <span className="inline-flex items-center gap-1.5">
                {AGENT_TO_PROVIDER[log.bot_name] && (
                  <AiAgentLogo
                    provider={AGENT_TO_PROVIDER[log.bot_name]}
                    agent={log.bot_name}
                    size={14}
                  />
                )}
                {log.bot_name}
              </span>
            ) : (
              '-'
            )}
          </TableCell>
        )}
        {visibleColumns.has('cache_status') && (
          <TableCell>
            {log.cache_status && (
              <Badge variant="outline">{log.cache_status}</Badge>
            )}
          </TableCell>
        )}
        {visibleColumns.has('upstream_host') && (
          <TableCell className="font-mono text-xs max-w-[150px] truncate">
            {log.upstream_host || '-'}
          </TableCell>
        )}
      </TableRow>
      {isExpanded && (
        <TableRow className="bg-muted/30 hover:bg-muted/30">
          <TableCell colSpan={visibleCount + 1} className="p-0">
            <ProxyLogInlineDetail log={log} />
          </TableCell>
        </TableRow>
      )}
    </>
  )
}

// --- Inline detail panel ---

function DetailField({
  label,
  value,
  mono,
  copyable,
}: {
  label: string
  value: string | number | null | undefined
  mono?: boolean
  copyable?: boolean
}) {
  if (value === null || value === undefined || value === '') return null
  const display = String(value)
  return (
    <div className="space-y-0.5 min-w-0">
      <p className="text-xs text-muted-foreground">{label}</p>
      <div className="flex items-center gap-1.5 min-w-0">
        <p
          className={`text-sm truncate ${mono ? 'font-mono' : ''}`}
          title={display}
        >
          {display}
        </p>
        {copyable && (
          <CopyButton
            value={display}
            minimal
            className="h-5 w-5 p-0 shrink-0 opacity-0 group-hover/detail:opacity-100 transition-opacity"
          />
        )}
      </div>
    </div>
  )
}

function ProxyLogInlineDetail({ log }: { log: ProxyLogResponse }) {
  const fullUrl = `${log.host}${log.path}${log.query_string ? `?${log.query_string}` : ''}`

  return (
    <div className="group/detail px-6 py-4 space-y-4">
      {/* Request line */}
      <div className="flex items-center gap-3">
        <Badge variant="outline" className="font-mono">
          {log.method}
        </Badge>
        <div className="flex-1 min-w-0">
          <div className="bg-muted rounded-md px-3 py-1.5 font-mono text-xs break-all flex items-center gap-2">
            <span className="flex-1">{fullUrl}</span>
            <CopyButton
              value={fullUrl}
              minimal
              className="h-5 w-5 p-0 shrink-0"
            />
          </div>
        </div>
        <Badge variant={getStatusVariant(log.status_code) as any}>
          {log.status_code}
        </Badge>
      </div>

      <Separator />

      {/* Details grid */}
      <div className="grid grid-cols-2 md:grid-cols-4 lg:grid-cols-6 gap-x-6 gap-y-3">
        <DetailField
          label="Timestamp"
          value={format(new Date(log.timestamp), 'PPpp')}
        />
        <DetailField
          label="Response Time"
          value={log.response_time_ms ? `${log.response_time_ms}ms` : null}
        />
        <DetailField
          label="Request Size"
          value={formatBytes(log.request_size_bytes)}
        />
        <DetailField
          label="Response Size"
          value={formatBytes(log.response_size_bytes)}
        />
        <DetailField label="Source" value={log.request_source} />
        <DetailField label="Routing" value={log.routing_status} />

        <DetailField label="Client IP" value={log.client_ip} mono copyable />
        <DetailField label="Device" value={log.device_type} />
        <DetailField
          label="Browser"
          value={
            log.browser
              ? `${log.browser}${log.browser_version ? ` ${log.browser_version}` : ''}`
              : null
          }
        />
        <DetailField label="OS" value={log.operating_system} />
        {log.is_bot && (
          <div className="space-y-0.5 min-w-0">
            <p className="text-xs text-muted-foreground">Bot</p>
            <div className="flex items-center gap-1.5 min-w-0">
              {log.bot_name && AGENT_TO_PROVIDER[log.bot_name] && (
                <AiAgentLogo
                  provider={AGENT_TO_PROVIDER[log.bot_name]}
                  agent={log.bot_name}
                  size={14}
                />
              )}
              <p className="text-sm truncate">
                {log.bot_name || 'Detected as bot'}
              </p>
            </div>
          </div>
        )}
        <DetailField label="Referrer" value={log.referrer} mono />

        <DetailField label="Project ID" value={log.project_id} />
        <DetailField label="Environment ID" value={log.environment_id} />
        <DetailField label="Deployment ID" value={log.deployment_id} />
        <DetailField label="Container" value={log.container_id} mono />
        <DetailField label="Upstream" value={log.upstream_host} mono />
        <DetailField label="Cache" value={log.cache_status} />
      </div>

      {/* Request ID */}
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span>Request ID:</span>
        <code className="font-mono">{log.request_id}</code>
        <CopyButton
          value={log.request_id}
          minimal
          className="h-4 w-4 p-0"
        />
      </div>

      {/* User Agent */}
      {log.user_agent && (
        <div className="space-y-1">
          <p className="text-xs text-muted-foreground">User Agent</p>
          <div className="bg-muted rounded-md px-3 py-1.5 font-mono text-xs break-all flex items-center gap-2">
            <span className="flex-1">{log.user_agent}</span>
            <CopyButton
              value={log.user_agent}
              minimal
              className="h-5 w-5 p-0 shrink-0"
            />
          </div>
        </div>
      )}

      {/* Error */}
      {log.error_message && (
        <div className="bg-destructive/10 border border-destructive/20 rounded-md px-3 py-2">
          <p className="text-xs font-medium text-destructive mb-1">Error</p>
          <p className="font-mono text-xs">{log.error_message}</p>
        </div>
      )}

      {/* Link to full detail page */}
      <div className="flex justify-end">
        <Link
          to={`/proxy-logs/${encodeURIComponent(log.request_id)}?ts=${encodeURIComponent(log.timestamp)}`}
          className="inline-flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={(e) => e.stopPropagation()}
        >
          View full details
          <ExternalLink className="h-3 w-3" />
        </Link>
      </div>
    </div>
  )
}

function getStatusVariant(statusCode: number) {
  if (statusCode >= 200 && statusCode < 300) return 'default'
  if (statusCode >= 300 && statusCode < 400) return 'secondary'
  return 'destructive'
}
