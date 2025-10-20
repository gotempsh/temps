'use client'

import {
  getEnvironmentsOptions,
  getOpentelemetryLogsOptions,
  getOpentelemetryTracesOptions,
  getTracePercentilesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AttributeFilter,
  OpentelemetryLogResponse,
  ProjectResponse,
  TraceResponse,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Calendar } from '@/components/ui/calendar'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { TimeField } from '@/components/ui/time-field'
import { cn } from '@/lib/utils'
import { TraceDetails } from '@/pages/TraceDetails'
import { useQuery } from '@tanstack/react-query'
import { format, subMinutes } from 'date-fns'
import {
  Activity,
  ArrowUpDown,
  Calendar as CalendarIcon,
  ExternalLink,
  Plus,
  Terminal,
  X,
} from 'lucide-react'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useMemo, useState } from 'react'
import { DateRange } from 'react-day-picker'
import {
  Link,
  Navigate,
  Route,
  Routes,
  useLocation,
  useParams,
  useSearchParams,
} from 'react-router-dom'
import { Input } from '../ui/input'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '../ui/tabs'

const QUICK_FILTERS = [
  { label: 'Last 30m', value: '30m' },
  { label: 'Last 1h', value: '1h' },
  { label: 'Last 3h', value: '3h' },
  { label: 'Last 6h', value: '6h' },
  { label: 'Last 12h', value: '12h' },
  { label: 'Last 24h', value: '24h' },
  { label: 'Custom', value: 'custom' },
] as const

type QuickFilter = (typeof QUICK_FILTERS)[number]['value']

interface Trace {
  id: string
  path: string
  spanCount: number
  duration: number
  timestamp: string
  timestampMs: number
  databaseDate: string
  attributes: Record<string, string>
}

function mapTraceResponseToTrace(trace: TraceResponse): Trace {
  const timestampMs = Math.floor(trace.start_time / 1_000_000) // Convert nanoseconds to milliseconds
  const date = new Date(timestampMs)

  return {
    id: trace.trace_id,
    path: trace.root_span_name,
    spanCount: 1, // TODO: Get actual span count from trace details if needed
    duration: trace.duration_ms,
    timestamp: format(date, 'hh:mm:ss a'),
    timestampMs: timestampMs,
    databaseDate: trace.database_date,
    attributes: trace.attributes as any as Record<string, string>,
  }
}

// Convert a Date to nanoseconds timestamp number
const dateToNanos = (date: Date) => {
  return date.getTime() * 1_000_000
}

function AttributesList({
  attributes,
}: {
  attributes: Record<string, string>
}) {
  const [isExpanded, setIsExpanded] = useState(false)
  const entries = Object.entries(attributes || {})

  if (entries.length === 0) return null

  const displayedEntries = isExpanded ? entries : entries.slice(0, 4)
  const hasMore = entries.length > 4

  return (
    <>
      {displayedEntries.map(([key, value]) => (
        <span
          key={key}
          className="inline-flex items-center rounded-md bg-muted px-2 py-0.5 text-xs font-medium"
        >
          {key}: {value}
        </span>
      ))}
      {hasMore && !isExpanded && (
        <span
          role="button"
          className="inline-flex items-center rounded-md bg-muted px-2 py-0.5 text-xs font-medium hover:bg-muted/80 cursor-pointer"
          onClick={(e) => {
            e.preventDefault()
            e.stopPropagation()
            setIsExpanded(true)
          }}
        >
          +{entries.length - 4}
        </span>
      )}
    </>
  )
}

function ProjectObservabilityTraces({
  project,
  environmentId,
  startDate,
  endDate,
  attributeFilters,
}: {
  project: ProjectResponse
  environmentId: number | undefined
  startDate: Date
  endDate: Date
  attributeFilters: AttributeFilter[]
}) {
  const [sortOrder, setSortOrder] = useState<
    'most-recent' | 'longest' | 'shortest'
  >('most-recent')
  const { data: percentiles, isLoading: percentilesLoading } = useQuery({
    ...getTracePercentilesOptions({
      path: {
        project_id: project.id,
      },
      query: {
        environment_id: environmentId,
        start_time_unix_nano: dateToNanos(startDate),
        end_time_unix_nano: dateToNanos(endDate),
      },
    }),
  })

  const { data: tracesData, isLoading: tracesLoading } = useQuery(
    getOpentelemetryTracesOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_time_unix_nano: dateToNanos(startDate),
        end_time_unix_nano: dateToNanos(endDate),
        limit: 10,
        environment_id: environmentId,
        attributes: JSON.stringify(attributeFilters || []) as any,
      },
    })
  )

  const traces = useMemo(
    () => (tracesData?.data ?? []).map(mapTraceResponseToTrace),
    [tracesData]
  )

  const metrics = useMemo(() => {
    if (!percentiles) return []

    return [
      {
        name: 'p99 Latency',
        value: percentiles.p99,
        delta: 0, // We don't have delta information from the API
      },
      {
        name: 'p95 Latency',
        value: percentiles.p95,
        delta: 0,
      },
      {
        name: 'p50 Latency',
        value: percentiles.p50,
        delta: 0,
      },
    ]
  }, [percentiles])

  const sortedTraces = useMemo(() => {
    return [...traces].sort((a, b) => {
      if (sortOrder === 'most-recent') {
        return new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime()
      }
      if (sortOrder === 'longest') {
        return b.duration - a.duration
      }
      return a.duration - b.duration
    })
  }, [sortOrder, traces])

  const maxDuration = useMemo(() => {
    return Math.max(...traces.map((trace: Trace) => trace.duration))
  }, [traces])
  if (percentilesLoading || tracesLoading) return <div>Loading...</div>
  if (!traces.length) {
    return (
      <Card className="p-6">
        <EmptyState
          icon={Activity}
          title="No traces found"
          description="No traces have been collected yet. Traces will appear here once your application starts generating them."
          action={
            <Button variant="outline">
              <Activity className="mr-2 h-4 w-4" />
              Learn About Tracing
            </Button>
          }
        />
      </Card>
    )
  }

  return (
    <div className="space-y-6">
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
        {metrics.map((metric) => (
          <Card key={metric.name}>
            <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
              <CardTitle className="text-sm font-medium">
                {metric.name}
              </CardTitle>
              {/* {metric.delta < 0 ? <TrendingDown className="h-4 w-4 text-green-500" /> : <TrendingUp className="h-4 w-4 text-red-500" />} */}
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">
                {metric.value.toFixed(2)}ms
              </div>
              {/* <p className={cn('text-xs', metric.delta < 0 ? 'text-green-500' : 'text-red-500')}>
								{metric.delta < 0 ? '↓' : '↑'} {Math.abs(metric.delta).toFixed(2)}ms from last period
							</p> */}
            </CardContent>
          </Card>
        ))}
      </div>

      <Card>
        <CardHeader className="pb-0">
          <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-2">
              <CardTitle>{sortedTraces.length} Traces</CardTitle>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">Sort:</span>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline" size="sm" className="text-sm">
                    {sortOrder === 'most-recent'
                      ? 'Most Recent'
                      : sortOrder === 'longest'
                        ? 'Longest'
                        : 'Shortest'}
                    <ArrowUpDown className="ml-2 h-4 w-4" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem onClick={() => setSortOrder('most-recent')}>
                    Most Recent
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setSortOrder('longest')}>
                    Longest
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setSortOrder('shortest')}>
                    Shortest
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="space-y-2 mt-4">
            {sortedTraces.map((trace) => (
              <Link
                key={trace.id}
                to={`${trace.databaseDate}/${trace.id}`}
                className="block group rounded-lg border bg-card p-3 text-card-foreground transition-colors hover:bg-accent"
              >
                <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                  <div className="flex-1 min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-medium truncate max-w-[200px] sm:max-w-none">
                        {trace.path}
                      </span>
                      <span className="inline-flex items-center rounded-md bg-muted px-2 py-0.5 text-xs font-medium">
                        {trace.spanCount} Span{trace.spanCount !== 1 && 's'}
                      </span>
                    </div>
                    <div className="mt-2 flex flex-wrap items-center gap-2">
                      <AttributesList attributes={trace.attributes} />
                    </div>
                    <div className="mt-2 flex items-center gap-4">
                      <div className="w-full sm:w-32 h-2 bg-muted rounded-full overflow-hidden">
                        <div
                          className="h-full bg-primary"
                          style={{
                            width: `${(trace.duration / maxDuration) * 100}%`,
                          }}
                        />
                      </div>
                      <span className="text-xs text-muted-foreground whitespace-nowrap">
                        {trace.duration.toFixed(2)}ms
                      </span>
                    </div>
                  </div>
                  <div className="flex items-center justify-between sm:flex-col sm:items-end gap-1">
                    <div className="flex items-center gap-2">
                      <span className="text-primary text-sm">Today</span>
                      <span className="text-sm">{trace.timestamp}</span>
                      <ExternalLink className="h-4 w-4 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity" />
                    </div>
                    <TimeAgo
                      date={trace.timestampMs}
                      className="text-xs text-muted-foreground"
                    />
                  </div>
                </div>
              </Link>
            ))}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

interface ProjectObservabilityProps {
  project: ProjectResponse
}

// interface AttributeFilter {
// 	key: string
// 	value: string
// }

export function ProjectObservability({ project }: ProjectObservabilityProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const location = useLocation()

  const [selectedEnvironment, setSelectedEnvironment] = useState<string>(
    searchParams.get('env') ?? 'all'
  )
  const [searchQuery, setSearchQuery] = useState(searchParams.get('q') ?? '')
  const [activeFilter, setActiveFilter] = useState<QuickFilter>(
    (searchParams.get('filter') as QuickFilter) ?? '30m'
  )
  const [dateRange, setDateRange] = useState<DateRange | undefined>(() => {
    const start = searchParams.get('start')
    const end = searchParams.get('end')
    if (start && end) {
      return {
        from: new Date(start),
        to: new Date(end),
      }
    }
    return undefined
  })
  const [attributeFilters, setAttributeFilters] = useState<AttributeFilter[]>(
    []
  )
  const [newFilterKey, setNewFilterKey] = useState('')
  const [newFilterValue, setNewFilterValue] = useState('')
  const [editingFilterIndex, setEditingFilterIndex] = useState<number | null>(
    null
  )
  const [isAddingFilter, setIsAddingFilter] = useState(false)

  const { data: environments } = useQuery(
    getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    })
  )

  // Get the date range based on the active filter or custom range
  const getDateRange = () => {
    const now = new Date()
    now.setMilliseconds(0) // Reset milliseconds to 0

    if (activeFilter === 'custom' && dateRange?.from) {
      const from = new Date(dateRange.from)
      from.setSeconds(0, 0) // Reset seconds and milliseconds to 0
      const to = dateRange.to ? new Date(dateRange.to) : now
      to.setSeconds(0, 0)
      return {
        startDate: from,
        endDate: to,
      }
    }

    const endDate = new Date(now)
    endDate.setSeconds(0, 0)
    let startDate: Date

    switch (activeFilter) {
      case '30m':
        startDate = subMinutes(endDate, 30)
        break
      case '1h':
        startDate = subMinutes(endDate, 60)
        break
      case '3h':
        startDate = subMinutes(endDate, 180)
        break
      case '6h':
        startDate = subMinutes(endDate, 360)
        break
      case '12h':
        startDate = subMinutes(endDate, 720)
        break
      case '24h':
        startDate = subMinutes(endDate, 1440)
        break
      default:
        startDate = subMinutes(endDate, 30)
    }

    startDate.setSeconds(0, 0)
    return { startDate, endDate }
  }

  // Update URL when filters change
  const updateFilters = (updates: {
    env?: string | null
    q?: string | null
    filter?: string | null
    start?: string | null
    end?: string | null
  }) => {
    const newParams = new URLSearchParams(searchParams)

    Object.entries(updates).forEach(([key, value]) => {
      if (value !== null && value !== undefined) {
        if (key === 'start' || key === 'end') {
          // Convert to nanoseconds for the URL
          const date = new Date(value)
          const nanosTimestamp = dateToNanos(date)
          newParams.set(key, nanosTimestamp.toString())
        } else {
          newParams.set(key, value)
        }
      } else {
        newParams.delete(key)
      }
    })

    setSearchParams(newParams)
  }

  // Handle environment change
  const handleEnvironmentChange = (value: string) => {
    setSelectedEnvironment(value)
    updateFilters({ env: value === 'all' ? null : value })
  }

  // Handle search query change
  const handleSearchChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const value = e.target.value
    setSearchQuery(value)
    updateFilters({ q: value || null })
  }

  // Handle quick filter change
  const handleFilterChange = (filter: QuickFilter) => {
    setActiveFilter(filter)
    if (filter !== 'custom') {
      setDateRange(undefined)
      updateFilters({ filter, start: null, end: null })
    } else {
      updateFilters({ filter })
    }
  }

  // Handle date range change
  const handleDateRangeChange = (range: DateRange | undefined) => {
    setDateRange(range)
    if (range?.from) {
      const from = new Date(range.from)
      from.setSeconds(0, 0)
      const to = range.to ? new Date(range.to) : new Date()
      to.setSeconds(0, 0)

      setActiveFilter('custom')
      updateFilters({
        filter: 'custom',
        start: from.toISOString().split('.')[0] + '.000',
        end: to.toISOString().split('.')[0] + '.000',
      })
    }
  }

  const activeTab = location.pathname.includes('/logs') ? 'logs' : 'traces'
  const { startDate, endDate } = getDateRange()

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">Observability</h2>
            <p className="text-sm text-muted-foreground">
              Monitor your application's logs and traces in real-time.
            </p>
          </div>
          <Select
            value={selectedEnvironment}
            onValueChange={handleEnvironmentChange}
          >
            <SelectTrigger className="w-[180px]">
              <SelectValue placeholder="Select environment" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All Environments</SelectItem>
              {environments?.map((env) => (
                <SelectItem key={env.id} value={env.id.toString()}>
                  {env.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
          <div className="flex items-center sm:justify-end gap-2">
            <div className="hidden sm:flex gap-1">
              {QUICK_FILTERS.slice(0, -1).map((filter) => (
                <Button
                  key={filter.value}
                  variant={
                    activeFilter === filter.value ? 'default' : 'outline'
                  }
                  size="sm"
                  onClick={() => handleFilterChange(filter.value)}
                >
                  {filter.label}
                </Button>
              ))}
            </div>
            <div className="sm:hidden">
              <Select
                value={activeFilter}
                onValueChange={(value) =>
                  handleFilterChange(value as QuickFilter)
                }
              >
                <SelectTrigger className="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {QUICK_FILTERS.slice(0, -1).map((filter) => (
                    <SelectItem key={filter.value} value={filter.value}>
                      {filter.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <Popover>
              <PopoverTrigger asChild>
                <Button
                  variant={activeFilter === 'custom' ? 'default' : 'outline'}
                  size="sm"
                  className={cn(
                    'min-w-[140px]',
                    !dateRange?.from && 'text-muted-foreground'
                  )}
                >
                  <CalendarIcon className="mr-2 h-4 w-4" />
                  {dateRange?.from ? (
                    dateRange.to ? (
                      <>
                        {format(dateRange.from, 'LLL dd, y HH:mm')} -{' '}
                        {format(dateRange.to, 'LLL dd, y HH:mm')}
                      </>
                    ) : (
                      format(dateRange.from, 'LLL dd, y HH:mm')
                    )
                  ) : (
                    <span>Custom range</span>
                  )}
                </Button>
              </PopoverTrigger>
              <PopoverContent className="w-auto p-0" align="end">
                <div className="p-4 space-y-4">
                  <Calendar
                    initialFocus
                    mode="range"
                    defaultMonth={startDate}
                    selected={dateRange}
                    onSelect={handleDateRangeChange}
                    numberOfMonths={2}
                  />
                  <div className="flex items-center gap-4">
                    <div className="flex-1 space-y-2">
                      <div className="text-sm font-medium">Start Time</div>
                      <TimeField
                        value={dateRange?.from}
                        onChange={(date: Date) => {
                          if (!dateRange?.from) return
                          const newFrom = new Date(dateRange.from)
                          newFrom.setHours(date.getHours())
                          newFrom.setMinutes(date.getMinutes())
                          handleDateRangeChange({
                            from: newFrom,
                            to: dateRange.to,
                          })
                        }}
                      />
                    </div>
                    <div className="flex-1 space-y-2">
                      <div className="text-sm font-medium">End Time</div>
                      <TimeField
                        value={dateRange?.to}
                        onChange={(date: Date) => {
                          if (!dateRange?.to) return
                          const newTo = new Date(dateRange.to)
                          newTo.setHours(date.getHours())
                          newTo.setMinutes(date.getMinutes())
                          handleDateRangeChange({
                            from: dateRange.from,
                            to: newTo,
                          })
                        }}
                      />
                    </div>
                  </div>
                </div>
              </PopoverContent>
            </Popover>
          </div>
        </div>
      </div>

      <Tabs value={activeTab} className="space-y-4">
        <div className="flex sm:flex-row sm:items-center sm:justify-between gap-4">
          <TabsList>
            <TabsTrigger value="traces" asChild>
              <Link
                to={`/projects/${project.slug}/observability/traces?${searchParams.toString()}`}
              >
                Traces
              </Link>
            </TabsTrigger>
            <TabsTrigger value="logs" asChild>
              <Link
                to={`/projects/${project.slug}/observability/logs?${searchParams.toString()}`}
              >
                Logs
              </Link>
            </TabsTrigger>
          </TabsList>
        </div>

        <Routes>
          <Route
            path="logs"
            element={
              <TabsContent value="logs" className="space-y-4 mt-4">
                {/* <div className="relative">
									<Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
									<Input placeholder="Search logs..." value={searchQuery} onChange={handleSearchChange} className="pl-9" />
								</div> */}
                <LogsContent
                  project={project}
                  environmentId={
                    selectedEnvironment === 'all'
                      ? undefined
                      : parseInt(selectedEnvironment, 10)
                  }
                  searchQuery={searchQuery}
                  startDate={startDate}
                  endDate={endDate}
                />
              </TabsContent>
            }
          />
          <Route
            path="traces/:databaseDate/:traceId"
            element={<TraceDetail project={project} />}
          />
          <Route path="" element={<Navigate to="traces" />} />
          <Route
            index
            path="traces"
            element={
              <TabsContent value="traces" className="space-y-4 mt-4">
                <div className="flex flex-wrap gap-2">
                  {attributeFilters.map((filter, index) => (
                    <Badge
                      key={index}
                      variant="secondary"
                      className="flex items-center gap-1 cursor-pointer hover:bg-accent"
                      onClick={() => {
                        setEditingFilterIndex(index)
                        setNewFilterKey(filter.key)
                        setNewFilterValue(filter.value as string)
                      }}
                    >
                      {filter.key}: {filter.value as string}
                      <X
                        className="h-3 w-3 cursor-pointer hover:text-destructive"
                        onClick={(e) => {
                          e.stopPropagation()
                          setAttributeFilters((filters) =>
                            filters.filter((_, i) => i !== index)
                          )
                        }}
                      />
                    </Badge>
                  ))}
                  <Popover
                    open={editingFilterIndex !== null || isAddingFilter}
                    onOpenChange={(open) => {
                      if (!open) {
                        setEditingFilterIndex(null)
                        setIsAddingFilter(false)
                        setNewFilterKey('')
                        setNewFilterValue('')
                      }
                    }}
                  >
                    <PopoverTrigger asChild>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setIsAddingFilter(true)}
                      >
                        <Plus className="h-4 w-4 mr-2" />
                        Add Filter
                      </Button>
                    </PopoverTrigger>
                    <PopoverContent className="w-80">
                      <div className="space-y-4">
                        <div className="flex gap-2">
                          <div className="flex-1 space-y-2">
                            <Label>Key</Label>
                            <Input
                              placeholder="Enter key..."
                              value={newFilterKey}
                              onChange={(e) => setNewFilterKey(e.target.value)}
                            />
                          </div>
                          <div className="flex-1 space-y-2">
                            <Label>Value</Label>
                            <Input
                              placeholder="Enter value..."
                              value={newFilterValue}
                              onChange={(e) =>
                                setNewFilterValue(e.target.value)
                              }
                            />
                          </div>
                        </div>
                        <Button
                          className="w-full"
                          disabled={!newFilterKey || !newFilterValue}
                          onClick={() => {
                            if (editingFilterIndex !== null) {
                              setAttributeFilters((filters) =>
                                filters.map((f, i) =>
                                  i === editingFilterIndex
                                    ? {
                                        key: newFilterKey,
                                        value: newFilterValue,
                                      }
                                    : f
                                )
                              )
                              setEditingFilterIndex(null)
                            } else {
                              setAttributeFilters((filters) => [
                                ...filters,
                                { key: newFilterKey, value: newFilterValue },
                              ])
                              setIsAddingFilter(false)
                            }
                            setNewFilterKey('')
                            setNewFilterValue('')
                          }}
                        >
                          {editingFilterIndex !== null
                            ? 'Update Filter'
                            : 'Add Filter'}
                        </Button>
                      </div>
                    </PopoverContent>
                  </Popover>
                </div>
                <TracesContent
                  project={project}
                  environmentId={
                    selectedEnvironment === 'all'
                      ? undefined
                      : parseInt(selectedEnvironment, 10)
                  }
                  startDate={startDate}
                  endDate={endDate}
                  attributeFilters={attributeFilters}
                />
              </TabsContent>
            }
          />
        </Routes>
      </Tabs>
    </div>
  )
}

function LogsContent({
  project,
  environmentId,
  searchQuery,
  startDate,
  endDate,
}: {
  project: ProjectResponse
  environmentId?: number
  searchQuery: string
  startDate: Date
  endDate: Date
}) {
  const { data: logsData } = useQuery(
    getOpentelemetryLogsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_time_unix_nano: dateToNanos(startDate),
        end_time_unix_nano: dateToNanos(endDate),
        limit: 10,
        environment_id: environmentId,
      },
    })
  )

  const logs = useMemo(() => logsData?.data ?? [], [logsData])
  const hasLogs = logs.length > 0

  const filteredLogs = useMemo(() => {
    if (!searchQuery) return logs
    return logs.filter(
      (log: OpentelemetryLogResponse) =>
        log.body.toLowerCase().includes(searchQuery.toLowerCase()) ||
        log.severity_text.toLowerCase().includes(searchQuery.toLowerCase()) ||
        ((log.attributes as { service_name?: string })?.service_name ?? '')
          .toLowerCase()
          .includes(searchQuery.toLowerCase())
    )
  }, [logs, searchQuery])

  return (
    <Card className="p-0">
      {!hasLogs ? (
        <EmptyState
          icon={Terminal}
          title="No logs found"
          description="No application logs have been collected yet. Logs will appear here once your application starts generating them."
          action={
            <Button variant="outline">
              <Terminal className="mr-2 h-4 w-4" />
              View Documentation
            </Button>
          }
        />
      ) : (
        filteredLogs.map((log: OpentelemetryLogResponse, index: number) => (
          <div
            key={index}
            className={cn(
              'flex flex-col gap-2 p-4 border-b last:border-0 hover:bg-muted/50 cursor-pointer',
              'transition-colors duration-200'
            )}
          >
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">
                {format(
                  new Date(Math.floor(log.timestamp / 1_000_000)),
                  'HH:mm:ss'
                )}
              </span>
              <span
                className={cn(
                  'text-xs px-2 py-0.5 rounded-full',
                  log.severity_text === 'error' && 'bg-red-100 text-red-700',
                  log.severity_text === 'info' && 'bg-blue-100 text-blue-700'
                )}
              >
                {log.severity_text}
              </span>
              <span className="text-sm font-medium">
                {(log.attributes as { service_name?: string })?.service_name ??
                  'unknown'}
              </span>
            </div>
            <p className="text-sm">{log.body}</p>
            {(log.attributes as any) &&
              Object.keys(log.attributes as any).length > 0 && (
                <pre className="text-xs bg-muted p-2 rounded-md overflow-x-auto">
                  {JSON.stringify(
                    log.attributes as any as Record<string, string>,
                    null,
                    2
                  )}
                </pre>
              )}
          </div>
        ))
      )}
    </Card>
  )
}

function TracesContent({
  project,
  environmentId,
  startDate,
  endDate,
  attributeFilters,
}: {
  project: ProjectResponse
  environmentId?: number
  startDate: Date
  endDate: Date
  attributeFilters: AttributeFilter[]
}) {
  return (
    <div className="space-y-6">
      <ProjectObservabilityTraces
        project={project}
        environmentId={environmentId}
        startDate={startDate}
        endDate={endDate}
        attributeFilters={attributeFilters}
      />
    </div>
  )
}

function TraceDetail({ project }: { project: ProjectResponse }) {
  const { databaseDate, traceId } = useParams<{
    databaseDate: string
    traceId: string
  }>()
  return (
    <div>
      <TraceDetails
        databaseDate={databaseDate as string}
        traceId={traceId as string}
        project={project}
      />
    </div>
  )
}
