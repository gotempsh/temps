// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse, ProxyLogResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getProxyLogsOptions,
  getProxyLogsQueryKey,
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
import { TimeRangeFilter } from '@/components/ui/time-range-filter'
import { RefreshButton } from '@/components/ui/refresh-button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { httpStatusClass } from '@/lib/http-status-class'
import { resolveTimeRange } from '@/lib/time-range-filter'
import {
  Callout,
  DataTable,
  PageHeader,
  PageState,
  RecordLink,
  Status,
  fmtDuration,
  fmtNumber,
  useUrlState,
  type DataTableColumn,
  type StatusTone,
} from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  FileSearch,
  SlidersHorizontal,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useLocation } from 'react-router'
import {
  DEFAULT_REQUEST_LOG_COLUMNS,
  REQUEST_LOG_COLUMNS,
  REQUEST_LOG_COLUMNS_STORAGE_KEY,
  type RequestLogColumn,
} from './request-log-columns'
import { TableColumnPicker } from '@/components/table/TableColumnPicker'
import { useTableColumns } from '@/components/table/useTableColumns'
import {
  captureListReturnState,
  useListScrollRestoration,
  type ListReturnState,
} from '@/components/table/list-navigation'
import { useWindowQuery } from '@/hooks/useWindowQuery'

interface ProxyLogsListProps {
  project: ProjectResponse
  listState?: ListReturnState | null
  onRememberList?: (state: ListReturnState) => void
  showEnvironmentFilter?: boolean
}

type SortField = 'timestamp' | 'status_code' | 'response_time_ms'

const HTTP_METHODS = [
  'GET',
  'POST',
  'PUT',
  'DELETE',
  'PATCH',
  'HEAD',
  'OPTIONS',
]

function positiveInteger(value: string | undefined, fallback: number) {
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback
}

function statusTone(code: number): StatusTone {
  const status = httpStatusClass(code)
  return status === '2xx'
    ? 'ok'
    : status === '4xx'
      ? 'warn'
      : status === '5xx'
        ? 'error'
        : 'idle'
}

export default function ProxyLogsList({
  project,
  listState,
  onRememberList,
  showEnvironmentFilter = true,
}: ProxyLogsListProps) {
  const { state, patch } = useUrlState()
  const location = useLocation()
  const url = location.pathname + location.search
  const container = useRef<HTMLDivElement>(null)
  // Reuse the exact query window on return. Resolving "24h" again would change
  // the query key, discard the cached rows and clamp the pane's scroll to zero.
  const [saved] = useState(() => {
    const candidate = listState
    return candidate?.url === url ? candidate : null
  })
  const [initialNow] = useState(Date.now)
  const [refreshedAt, setRefreshedAt] = useState<number | null>(null)
  const page = positiveInteger(state.page, 1)
  const requestedLimit = positiveInteger(state.limit, 25)
  const limit = [10, 25, 50, 100].includes(requestedLimit) ? requestedLimit : 25
  const sortBy: SortField =
    state.sort_by === 'status_code' || state.sort_by === 'response_time_ms'
      ? state.sort_by
      : 'timestamp'
  const sortOrder = state.sort_order === 'asc' ? 'asc' : 'desc'
  const method = HTTP_METHODS.includes(state.method ?? '')
    ? state.method!
    : 'all'
  const statusCode = /^[1-5]\d{2}$/.test(state.status_code ?? '')
    ? state.status_code!
    : 'all'
  const environment = positiveInteger(state.environment, 0)
    ? state.environment!
    : 'all'
  const showBots =
    state.show_bots === 'no' || state.show_bots === 'yes'
      ? state.show_bots
      : 'all'
  const aiAgent = state.ai_agent ?? ''
  const aiProvider = state.ai_provider ?? ''
  const isAiAgent = state.is_ai_agent === 'true'
  const pathFilter = state.path ?? ''
  const hasAiContext = !!aiAgent || !!aiProvider || isAiAgent
  const timeRange =
    state.time_range || (hasAiContext || pathFilter ? '7d' : '24h')
  const activeFilterCount = [
    method !== 'all',
    statusCode !== 'all',
    environment !== 'all',
    showBots !== 'all',
  ].filter(Boolean).length
  const hasFilters =
    activeFilterCount > 0 || timeRange !== '24h' || hasAiContext || !!pathFilter
  const showFilters =
    state.filters === 'open' ||
    (state.filters !== 'closed' && activeFilterCount > 0)
  const { visibleKeys, changeColumns, resetColumns } = useTableColumns({
    options: REQUEST_LOG_COLUMNS,
    defaults: DEFAULT_REQUEST_LOG_COLUMNS,
    storageKey: REQUEST_LOG_COLUMNS_STORAGE_KEY,
  })

  const dateRange = useMemo(() => {
    if (refreshedAt === null && saved?.timeRange === timeRange) {
      return saved.dateRange
    }
    const range = resolveTimeRange(timeRange, refreshedAt ?? initialNow)
    return { from: range.from, to: range.to }
  }, [timeRange, refreshedAt, initialNow, saved])

  const { data: environmentsData, isLoading: environmentsLoading } = useQuery(
    getEnvironmentsOptions({ path: { project_id: project.id } })
  )
  const query = useWindowQuery({
    ...getProxyLogsOptions({
      query: {
        page,
        page_size: limit,
        project_id: project.id,
        status_code: statusCode !== 'all' ? Number(statusCode) : undefined,
        method: method !== 'all' ? method : undefined,
        start_date: dateRange.from,
        end_date: dateRange.to,
        environment_id: environment !== 'all' ? Number(environment) : undefined,
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
        sort_by: sortBy,
        sort_order: sortOrder,
      },
    }),
    // A refresh moves the window, so the key changes: keep the current rows
    // and count on screen until the next ones arrive.
  })
  const { data: logs, isLoading, error } = query
  const totalPages = Math.max(1, Math.ceil((logs?.total ?? 0) / limit))

  useEffect(() => {
    if (query.isSuccess && page > totalPages)
      patch({ page: totalPages === 1 ? undefined : totalPages })
  }, [query.isSuccess, page, totalPages, patch])

  useListScrollRestoration(container, saved, !!logs, url)
  const rememberList = () =>
    onRememberList?.(
      captureListReturnState(container.current, { url, timeRange, dateRange })
    )
  const changeFilter = (key: string, value: string) =>
    patch({ [key]: value === 'all' ? undefined : value, page: undefined })
  const handleMethodChange = (value: string) => changeFilter('method', value)
  const handleStatusCodeChange = (value: string) =>
    changeFilter('status_code', value)
  const handleEnvironmentChange = (value: string) =>
    changeFilter('environment', value)
  const handleShowBotsChange = (value: string) =>
    changeFilter('show_bots', value)
  const handleTimeRangeChange = (value: string) =>
    patch({ time_range: value, page: undefined })
  const setPage = (value: number) =>
    patch({ page: value > 1 ? value : undefined })
  const setAiAgent = (value: string) => patch({ ai_agent: value })
  const setAiProvider = (value: string) => patch({ ai_provider: value })
  const setIsAiAgent = (value: boolean) =>
    patch({ is_ai_agent: value ? true : undefined })
  const setPathFilter = (value: string) => patch({ path: value })
  const clearFilters = () =>
    patch({
      time_range: '24h',
      method: undefined,
      status_code: undefined,
      environment: undefined,
      show_bots: undefined,
      ai_agent: undefined,
      ai_provider: undefined,
      is_ai_agent: undefined,
      path: undefined,
      page: undefined,
    })
  const handleSort = (field: SortField) =>
    patch({
      sort_by: field,
      sort_order: sortBy === field && sortOrder === 'desc' ? 'asc' : 'desc',
      page: undefined,
    })

  const renderValue = (key: RequestLogColumn, log: ProxyLogResponse) => {
    switch (key) {
      case 'path': {
        const detailParams = new URLSearchParams({ ts: log.timestamp })
        return (
          <RecordLink
            to={`/projects/${project.slug}/request-logs/${encodeURIComponent(log.request_id)}?${detailParams}`}
            state={{ listReturnTo: url }}
            onClick={rememberList}
            aria-label={`View ${log.method} ${log.path} request details`}
            title={`${log.host}${log.path}${log.query_string ? `?${log.query_string}` : ''}`}
            className="font-mono"
          >
            <span className="block max-w-80 truncate">{log.path}</span>
          </RecordLink>
        )
      }
      case 'timestamp':
        return (
          <time dateTime={log.timestamp} title={log.timestamp}>
            {format(new Date(log.timestamp), 'MMM dd, HH:mm:ss')}
          </time>
        )
      case 'method':
        return <Badge variant="outline">{log.method}</Badge>
      case 'status_code':
        return (
          <Status
            tone={statusTone(log.status_code)}
            label={String(log.status_code)}
          />
        )
      case 'routing_status':
        return <Badge variant="secondary">{log.routing_status}</Badge>
      case 'response_time_ms':
        return log.response_time_ms == null
          ? '—'
          : fmtDuration(log.response_time_ms)
      case 'user_agent':
        return log.bot_name ? (
          <span className="inline-flex items-center gap-1.5">
            <AiAgentLogo agent={log.bot_name} size={14} />
            {log.bot_name}
          </span>
        ) : (
          <span
            className="block max-w-64 truncate"
            title={log.user_agent ?? undefined}
          >
            {log.user_agent || '—'}
          </span>
        )
      case 'environment_id':
        return (
          environmentsData?.find((env) => env.id === log.environment_id)
            ?.name ??
          (log.environment_id == null ? '—' : String(log.environment_id))
        )
      default:
        return (
          <span
            className={`block max-w-64 truncate ${key === 'host' || key === 'client_ip' ? 'font-mono' : ''}`}
            title={log[key] ?? undefined}
          >
            {log[key] || '—'}
          </span>
        )
    }
  }

  const columns: DataTableColumn<ProxyLogResponse>[] =
    REQUEST_LOG_COLUMNS.filter((column) =>
      visibleKeys.includes(column.key)
    ).map(({ key, label }) => {
      const sortable =
        key === 'timestamp' ||
        key === 'status_code' ||
        key === 'response_time_ms'
      const active = sortBy === key
      const Icon = active
        ? sortOrder === 'asc'
          ? ArrowUp
          : ArrowDown
        : ArrowUpDown
      return {
        key,
        header: sortable ? (
          <button
            type="button"
            className="inline-flex min-h-9 items-center gap-1 rounded-sm hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label={`Sort by ${label}, ${active && sortOrder === 'desc' ? 'ascending' : 'descending'}`}
            onClick={() => handleSort(key)}
          >
            {label}
            <Icon className="size-3.5" aria-hidden="true" />
          </button>
        ) : (
          label
        ),
        ariaSort:
          sortable && active
            ? sortOrder === 'asc'
              ? 'ascending'
              : 'descending'
            : undefined,
        className: `whitespace-nowrap ${key === 'response_time_ms' ? 'text-right tabular-nums' : ''} ${key === 'timestamp' ? 'tabular-nums text-muted-foreground' : ''}`,
        render: (log) => renderValue(key, log),
      }
    })

  return (
    <div ref={container} className="w-full min-w-0 space-y-6">
      <PageHeader
        headingLevel="h2"
        title="Request logs"
        description={
          logs
            ? `${fmtNumber(logs.total)} requests`
            : 'Browse requests handled by this project.'
        }
        actions={
          <RefreshButton
            queryKeys={[
              getProxyLogsQueryKey({ query: { project_id: project.id } }),
            ]}
            // A preset window ("Last 24 hours") is resolved again from now; a
            // custom one keeps its dates and is simply refetched.
            onRefresh={() => setRefreshedAt(Date.now())}
          />
        }
      />
      <div className="flex flex-wrap items-center gap-2">
        <fieldset aria-label="Time range" className="min-w-0">
          <TimeRangeFilter value={timeRange} onChange={handleTimeRangeChange} />
        </fieldset>
        <Button
          type="button"
          variant={showFilters ? 'secondary' : 'outline'}
          size="sm"
          aria-expanded={showFilters}
          aria-controls="request-log-filters"
          onClick={() => patch({ filters: showFilters ? 'closed' : 'open' })}
        >
          <SlidersHorizontal className="mr-2 size-4" aria-hidden="true" />
          Filters
          {activeFilterCount > 0 && (
            <Badge variant="secondary" className="ml-2 tabular-nums">
              {activeFilterCount}
            </Badge>
          )}
        </Button>
        <div className="ml-auto">
          <TableColumnPicker
            options={REQUEST_LOG_COLUMNS}
            visibleKeys={visibleKeys}
            onChange={changeColumns}
            onReset={resetColumns}
          />
        </div>
      </div>
      {/* Collapsible advanced filters — hidden by default */}
      {showFilters && (
        <div
          id="request-log-filters"
          className="rounded-lg border bg-muted/30 p-3 sm:p-4"
        >
          <div className="flex flex-wrap items-end gap-2 sm:gap-3">
            <div className="flex flex-col gap-1">
              <label
                htmlFor="filter-method"
                className="text-xs font-medium text-muted-foreground"
              >
                Method
              </label>
              <Select value={method} onValueChange={handleMethodChange}>
                <SelectTrigger
                  id="filter-method"
                  className="w-full sm:w-[140px]"
                >
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
                <SelectTrigger
                  id="filter-status"
                  className="w-full sm:w-[180px]"
                >
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
                    <SelectItem value="422">
                      422 Unprocessable Entity
                    </SelectItem>
                    <SelectItem value="429">429 Too Many Requests</SelectItem>
                  </SelectGroup>
                  <SelectGroup>
                    <SelectLabel>Server Error (5xx)</SelectLabel>
                    <SelectItem value="500">
                      500 Internal Server Error
                    </SelectItem>
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
                {environmentsLoading ? (
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

      {error && logs && (
        <Callout tone="error" title="Request logs could not refresh">
          <div className="flex flex-wrap items-center gap-3">
            <span>Showing previously loaded requests.</span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void query.refetch()}
            >
              Retry
            </Button>
          </div>
        </Callout>
      )}
      {error && !logs ? (
        <div className="rounded-lg border bg-card text-card-foreground">
          <PageState
            size="compact"
            variant="failed"
            icon={AlertCircle}
            title="Could not load request logs"
            description="Try again to load requests for this project."
            action={<Button onClick={() => void query.refetch()}>Retry</Button>}
          />
        </div>
      ) : !isLoading && logs?.logs.length === 0 ? (
        <div className="rounded-lg border bg-card text-card-foreground">
          <PageState
            size="compact"
            variant="empty"
            icon={FileSearch}
            title={
              hasFilters ? 'No matching request logs' : 'No request logs yet'
            }
            description={
              hasFilters
                ? 'Try another time range or clear the filters.'
                : 'Requests handled by this project will appear here.'
            }
            action={
              hasFilters ? (
                <Button variant="outline" onClick={clearFilters}>
                  Clear filters
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : (
        <DataTable
          aria-label="Request logs"
          columns={columns}
          rows={logs?.logs ?? []}
          rowKey={(log) => log.request_id}
          isLoading={isLoading}
        />
      )}
      {logs && (
        <ResponsivePagination
          page={page}
          pageSize={limit}
          total={logs.total}
          totalPages={totalPages}
          onPageChange={setPage}
          onPageSizeChange={(value) =>
            patch({
              limit: value === 25 ? undefined : value,
              page: undefined,
            })
          }
          pageSizeOptions={[10, 25, 50, 100]}
          ariaLabel="Request log pagination"
          pageSizeAriaLabel="Request logs per page"
        />
      )}
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
