import {
  getEnvironmentsOptions,
  getEventsCountOptions,
  getHourlyVisitsOptions,
  hasAnalyticsEventsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import {
  AiAgentsChart,
  AnalyticsMetrics,
  BrowsersChart,
  ChannelsChart,
  DevicesChart,
  LanguagesChart,
  LocationsChart,
  OperatingSystemChart,
  PagesChart,
  ReferrersChart,
  UTMCampaignsChart,
} from '@/components/analytics/overview'
import { AiAgentsDetail } from '@/components/analytics/AiAgentsDetail'
import { VisitorGlobePage } from '@/components/analytics/VisitorGlobe'
import { LiveGlobePage } from '@/components/analytics/LiveGlobe'
import { PageFlow } from '@/components/analytics/PageFlow'
import { PageDetail } from '@/components/analytics/PageDetail'
import { Pages } from '@/components/analytics/Pages'
import { SessionReplays } from '@/components/analytics/SessionReplays'
import { FunnelDetail } from '@/components/funnel/FunnelDetail'
import { FunnelManagement } from '@/components/funnel/FunnelManagement'
import { LiveVisitors } from '@/pages/LiveVisitors'
import { Button } from '@/components/ui/button'
import { Calendar } from '@/components/ui/calendar'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { CodeBlock } from '@/components/ui/code-block'
import { CopyButton } from '@/components/ui/copy-button'
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  SetupWizardShell,
  WizardStepId,
} from '@/components/project/setup/SetupWizardShell'
import VisitorAnalytics from '@/components/visitors/VisitorAnalytics'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { cn } from '@/lib/utils'
import { CreateFunnel } from '@/pages/CreateFunnel'
import { EditFunnel } from '@/pages/EditFunnel'
import RequestLogs from '@/pages/RequestLogs'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  getDateRangeFromFilter,
  QUICK_FILTERS,
  type QuickFilter,
  type AnalyticsDateFilter,
} from '@/hooks/useAnalyticsDateRange'
import {
  ArrowLeft,
  ArrowRight,
  Calendar as CalendarIcon,
  Check,
  FileCode,
  Globe,
  Info,
  Loader2,
  RefreshCw,
  RotateCcw,
  Terminal,
} from 'lucide-react'
import * as React from 'react'
import { DateRange } from 'react-day-picker'
import {
  Route,
  Routes,
  useLocation,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router-dom'
import { EventDetail } from '@/components/analytics/EventDetail'
import {
  DimensionList,
  isDimensionKey,
  type DimensionKey,
} from '@/components/analytics/DimensionList'
import {
  SegmentVisitors,
  segmentSupportsVisitors,
} from '@/components/analytics/SegmentVisitors'

import { Badge } from '@/components/ui/badge'
import { Line, LineChart, XAxis, YAxis } from 'recharts'

const chartConfig2 = {
  count: {
    label: 'Count',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

interface VisitorChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  onZoom?: (from: Date, to: Date) => void
}

export function VisitorChart({
  project,
  startDate,
  endDate,
  environment,
  onZoom,
}: VisitorChartProps) {
  const [aggregationLevel, setAggregationLevel] = React.useState<
    'events' | 'sessions' | 'visitors'
  >('visitors')

  // Brush zoom state — track timestamps for zoom + pixel X for overlay
  const [refAreaLeft, setRefAreaLeft] = React.useState<number | null>(null)
  const [refAreaRight, setRefAreaRight] = React.useState<number | null>(null)
  const [dragPixelLeft, setDragPixelLeft] = React.useState<number | null>(null)
  const [dragPixelRight, setDragPixelRight] = React.useState<number | null>(
    null
  )
  const isDragging = React.useRef(false)
  const chartContainerRef = React.useRef<HTMLDivElement>(null)

  const { data, isLoading, error } = useQuery({
    ...getHourlyVisitsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        aggregation_level: aggregationLevel,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = React.useMemo(() => {
    if (!data || !startDate || !endDate) return []

    // Calculate the range in days
    const rangeInDays = Math.ceil(
      (endDate.getTime() - startDate.getTime()) / (1000 * 60 * 60 * 24)
    )

    // Check if start and end are on the same day
    const sameDay = startDate.toDateString() === endDate.toDateString()

    return data.map((item) => {
      // Parse the date string (format: "2025-10-05 19:00")
      const date = new Date(item.date.replace(' ', 'T'))

      let formattedDate: string

      if (sameDay || rangeInDays <= 1) {
        // Same day or Last 24 hours: show only hour
        formattedDate = date.toLocaleString('en-US', {
          hour: 'numeric',
          hour12: true,
        })
      } else if (rangeInDays <= 7) {
        // Last 7 days (multiple days): show month, day and hour
        formattedDate = date.toLocaleString('en-US', {
          month: 'short',
          day: 'numeric',
          hour: 'numeric',
          hour12: true,
        })
      } else if (rangeInDays <= 30) {
        // Last 30 days: show month and day
        formattedDate = date.toLocaleString('en-US', {
          month: 'short',
          day: 'numeric',
        })
      } else {
        // More than 30 days: show month, day and year
        formattedDate = date.toLocaleString('en-US', {
          month: 'short',
          day: 'numeric',
          year: '2-digit',
        })
      }

      return {
        date: formattedDate,
        timestamp: date.getTime(),
        count: item.count,
      }
    })
  }, [data, startDate, endDate])

  // Helper: get pixel X relative to chart container from a recharts event
  const getPixelX = React.useCallback((e: any): number | null => {
    if (!e?.chartX) return null
    return e.chartX
  }, [])

  const handleMouseDown = React.useCallback(
    (e: any) => {
      if (!e || !onZoom) return
      const timestamp = e.activePayload?.[0]?.payload?.timestamp
      const px = getPixelX(e)
      if (timestamp && px != null) {
        isDragging.current = true
        setRefAreaLeft(timestamp)
        setRefAreaRight(null)
        setDragPixelLeft(px)
        setDragPixelRight(null)
      }
    },
    [onZoom, getPixelX]
  )

  const handleMouseMove = React.useCallback(
    (e: any) => {
      if (!isDragging.current || !e) return
      const timestamp = e.activePayload?.[0]?.payload?.timestamp
      const px = getPixelX(e)
      if (timestamp) {
        setRefAreaRight(timestamp)
      }
      if (px != null) {
        setDragPixelRight(px)
      }
    },
    [getPixelX]
  )

  const handleMouseUp = React.useCallback(() => {
    if (!isDragging.current || refAreaLeft == null || refAreaRight == null) {
      isDragging.current = false
      setRefAreaLeft(null)
      setRefAreaRight(null)
      setDragPixelLeft(null)
      setDragPixelRight(null)
      return
    }
    isDragging.current = false

    const left = Math.min(refAreaLeft, refAreaRight)
    const right = Math.max(refAreaLeft, refAreaRight)

    setRefAreaLeft(null)
    setRefAreaRight(null)
    setDragPixelLeft(null)
    setDragPixelRight(null)

    // Require a minimum drag distance (at least 2 data points apart)
    if (right - left < 1000 * 60 * 30) {
      return
    }

    onZoom?.(new Date(left), new Date(right))
  }, [refAreaLeft, refAreaRight, onZoom])

  // Compute the overlay position from pixel coordinates
  const selectionOverlay = React.useMemo(() => {
    if (dragPixelLeft == null || dragPixelRight == null) return null
    const left = Math.min(dragPixelLeft, dragPixelRight)
    const width = Math.abs(dragPixelRight - dragPixelLeft)
    if (width < 4) return null
    return { left, width }
  }, [dragPixelLeft, dragPixelRight])

  const getAggregationLabel = () => {
    switch (aggregationLevel) {
      case 'events':
        return 'Page Views'
      case 'sessions':
        return 'Sessions'
      case 'visitors':
        return 'Visitors'
    }
  }

  const getChartTitle = () => {
    if (!startDate || !endDate) return getAggregationLabel()

    const rangeInDays = Math.ceil(
      (endDate.getTime() - startDate.getTime()) / (1000 * 60 * 60 * 24)
    )
    const sameDay = startDate.toDateString() === endDate.toDateString()

    if (sameDay || rangeInDays <= 1) {
      return `Hourly ${getAggregationLabel()}`
    } else {
      return getAggregationLabel()
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <CardTitle className="text-base sm:text-lg">
          {getChartTitle()}
        </CardTitle>
        <div className="flex items-center gap-2">
          {onZoom && (
            <span className="text-xs text-muted-foreground hidden sm:inline">
              Drag on chart to zoom
            </span>
          )}
          <div className="flex gap-1.5 sm:gap-2">
            <Badge
              variant={aggregationLevel === 'events' ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setAggregationLevel('events')}
            >
              Events
            </Badge>
            <Badge
              variant={aggregationLevel === 'sessions' ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setAggregationLevel('sessions')}
            >
              Sessions
            </Badge>
            <Badge
              variant={aggregationLevel === 'visitors' ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setAggregationLevel('visitors')}
            >
              Visitors
            </Badge>
          </div>
        </div>
      </div>
      {isLoading ? (
        <div className="h-[250px] w-full flex items-center justify-center">
          <div className="text-sm text-muted-foreground">
            Loading chart data...
          </div>
        </div>
      ) : error ? (
        <div className="h-[250px] w-full flex items-center justify-center">
          <div className="text-sm text-red-500">Failed to load chart data</div>
        </div>
      ) : !chartData.length ? (
        <div className="h-[250px] w-full flex items-center justify-center">
          <div className="text-sm text-muted-foreground">
            No data available for the selected period
          </div>
        </div>
      ) : (
        <div ref={chartContainerRef} className="relative">
          {selectionOverlay && (
            <div
              className="absolute top-0 bottom-0 bg-primary/10 dark:bg-primary/20 border-x border-primary/30 dark:border-primary/40 pointer-events-none z-10 transition-none"
              style={{
                left: selectionOverlay.left,
                width: selectionOverlay.width,
              }}
            />
          )}
          <ChartContainer
            config={chartConfig2}
            className={cn('h-[250px] w-full', onZoom && 'cursor-crosshair')}
          >
            <LineChart
              accessibilityLayer
              data={chartData}
              margin={{
                left: 12,
                right: 12,
                top: 12,
                bottom: 12,
              }}
              onMouseDown={onZoom ? handleMouseDown : undefined}
              onMouseMove={onZoom ? handleMouseMove : undefined}
              onMouseUp={onZoom ? handleMouseUp : undefined}
            >
              <XAxis
                dataKey="date"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                minTickGap={32}
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                tickFormatter={(value) => value.toLocaleString()}
              />
              <ChartTooltip cursor={false} content={<ChartTooltipContent />} />
              <Line
                dataKey="count"
                type="monotone"
                stroke="var(--color-count)"
                strokeWidth={2}
                dot={false}
              />
            </LineChart>
          </ChartContainer>
        </div>
      )}
    </div>
  )
}

export interface AnalyticsFiltersProps {
  project: ProjectResponse
  activeFilter: QuickFilter
  dateRange: DateRange | undefined
  selectedEnvironment: number | undefined
  onFilterChange: (filter: QuickFilter) => void
  onDateRangeChange: (range: DateRange | undefined) => void
  onEnvironmentChange: (environment: number | undefined) => void
  onRefresh: () => void
  isRefreshing: boolean
}

export function AnalyticsFilters({
  project,
  activeFilter,
  dateRange,
  selectedEnvironment,
  onFilterChange,
  onDateRangeChange,
  onEnvironmentChange,
  onRefresh,
  isRefreshing,
}: AnalyticsFiltersProps) {
  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  return (
    <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-2">
      <Select
        value={selectedEnvironment?.toString()}
        onValueChange={(value) =>
          onEnvironmentChange(value ? parseInt(value) : undefined)
        }
      >
        <SelectTrigger className="w-[200px]">
          <SelectValue placeholder="All environments" />
        </SelectTrigger>
        <SelectContent>
          {environments?.map((env) => (
            <SelectItem key={env.id} value={env.id.toString()}>
              {env.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>

      <div className="flex items-center sm:justify-end gap-2">
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={onRefresh}
            disabled={isRefreshing}
          >
            <RefreshCw
              className={cn('h-4 w-4', isRefreshing && 'animate-spin')}
            />
            Refresh
          </Button>
          <div className="hidden sm:flex gap-1">
            {QUICK_FILTERS.slice(0, -1).map((filter) => (
              <Button
                key={filter.value}
                variant={activeFilter === filter.value ? 'default' : 'outline'}
                size="sm"
                onClick={() => onFilterChange(filter.value)}
              >
                {filter.label}
              </Button>
            ))}
          </div>
          <div className="sm:hidden">
            <Select value={activeFilter} onValueChange={onFilterChange}>
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
                  'sm:min-w-[140px]',
                  !dateRange?.from && 'text-muted-foreground'
                )}
              >
                <CalendarIcon className="h-4 w-4 sm:mr-2" />
                <span className="hidden sm:inline">
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
                    'Custom range'
                  )}
                </span>
                <span className="sm:hidden">
                  {dateRange?.from ? format(dateRange.from, 'MM/dd') : 'Custom'}
                </span>
              </Button>
            </PopoverTrigger>
            <PopoverContent className="w-auto p-0" align="end">
              <Calendar
                autoFocus
                mode="range"
                defaultMonth={
                  new Date(new Date().setMonth(new Date().getMonth() - 1))
                }
                selected={dateRange}
                onSelect={onDateRangeChange}
                numberOfMonths={
                  typeof window !== 'undefined' && window.innerWidth < 640
                    ? 1
                    : 2
                }
                disabled={[
                  (date) => date > new Date(),
                  {
                    before: new Date(
                      new Date().setMonth(new Date().getMonth() - 1)
                    ),
                  },
                ]}
                endMonth={new Date()}
                startMonth={
                  new Date(new Date().setMonth(new Date().getMonth() - 1))
                }
              />
              <div className="border-t p-3 flex items-end gap-4">
                <div className="flex-1 space-y-1">
                  <Label className="text-xs text-muted-foreground">
                    Start time
                  </Label>
                  <Input
                    type="time"
                    className="h-8 text-xs"
                    value={
                      dateRange?.from
                        ? format(dateRange.from, 'HH:mm')
                        : '00:00'
                    }
                    onChange={(e) => {
                      if (!dateRange?.from) return
                      const [hours, minutes] = e.target.value
                        .split(':')
                        .map(Number)
                      const updated = new Date(dateRange.from)
                      updated.setHours(hours, minutes, 0, 0)
                      onDateRangeChange({
                        from: updated,
                        to: dateRange.to,
                      })
                    }}
                    disabled={!dateRange?.from}
                  />
                </div>
                <div className="flex-1 space-y-1">
                  <Label className="text-xs text-muted-foreground">
                    End time
                  </Label>
                  <Input
                    type="time"
                    className="h-8 text-xs"
                    value={
                      dateRange?.to ? format(dateRange.to, 'HH:mm') : '23:59'
                    }
                    onChange={(e) => {
                      if (!dateRange?.to) return
                      const [hours, minutes] = e.target.value
                        .split(':')
                        .map(Number)
                      const updated = new Date(dateRange.to)
                      updated.setHours(hours, minutes, 59, 999)
                      onDateRangeChange({
                        from: dateRange.from,
                        to: updated,
                      })
                    }}
                    disabled={!dateRange?.to}
                  />
                </div>
              </div>
            </PopoverContent>
          </Popover>
        </div>
      </div>
    </div>
  )
}

// Pages Tab Component
interface PagesTabProps {
  project: ProjectResponse
}

function PagesTab({ project }: PagesTabProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const selectedPagePath = searchParams.get('path')

  // Restore date filter from URL search params (preserves context from overview)
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
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  const handleRefresh = React.useCallback(() => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as string
        return !!(
          key &&
          typeof key === 'string' &&
          key.includes('getPagePaths')
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  // Sync date filter to URL search params (preserves path param)
  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
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

  const handleBackToList = React.useCallback(() => {
    // Preserve date filter params when going back to list
    const params = new URLSearchParams()
    params.set('filter', dateFilter.quickFilter)
    if (
      dateFilter.quickFilter === 'custom' &&
      dateFilter.dateRange?.from &&
      dateFilter.dateRange?.to
    ) {
      params.set('from', dateFilter.dateRange.from.toISOString())
      params.set('to', dateFilter.dateRange.to.toISOString())
    }
    setSearchParams(params)
  }, [dateFilter, setSearchParams])

  return (
    <div className="space-y-6">
      {/* Date Filter and Environment Selector */}
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
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      {/* Show PageDetail when a path is selected, otherwise show Pages list */}
      {selectedPagePath ? (
        <PageDetail
          project={project}
          pagePath={selectedPagePath}
          startDate={startDate}
          endDate={endDate}
          environment={selectedEnvironment}
          onBack={handleBackToList}
        />
      ) : (
        <Pages
          project={project}
          startDate={startDate}
          endDate={endDate}
          environment={selectedEnvironment}
        />
      )}
    </div>
  )
}

// Event Detail Tab Component
interface EventDetailTabProps {
  project: ProjectResponse
}

function EventDetailTab({ project }: EventDetailTabProps) {
  const { eventName: rawEventName } = useParams<{ eventName: string }>()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const eventName = rawEventName ? decodeURIComponent(rawEventName) : ''

  // Restore date filter from URL search params (preserves context from overview).
  // Supports both quick filters (today/24hours/7days/30days) and custom ranges.
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
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  // Sync date filter to URL so deep links and back-navigation keep the range.
  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
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
        const key = query.queryKey[0] as string
        return !!(
          key &&
          typeof key === 'string' &&
          (key.includes('getEventDetail') || key.includes('getEventVisitors'))
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  if (!eventName) {
    return (
      <div className="p-8 text-center">
        <p className="text-sm text-muted-foreground">No event specified</p>
      </div>
    )
  }

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
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      <EventDetail
        project={project}
        eventName={eventName}
        startDate={startDate}
        endDate={endDate}
        environment={selectedEnvironment}
        onBack={() => {
          // Preserve the current date filter on the way back to the overview.
          const params = new URLSearchParams()
          params.set('filter', dateFilter.quickFilter)
          if (
            dateFilter.quickFilter === 'custom' &&
            dateFilter.dateRange?.from &&
            dateFilter.dateRange?.to
          ) {
            params.set('from', dateFilter.dateRange.from.toISOString())
            params.set('to', dateFilter.dateRange.to.toISOString())
          }
          const qs = params.toString()
          navigate(
            `/projects/${project.slug}/analytics${qs ? `?${qs}` : ''}`
          )
        }}
      />
    </div>
  )
}

// Dimension List Tab — generic "view all" page for any property breakdown.
interface DimensionTabProps {
  project: ProjectResponse
}

function DimensionTab({ project }: DimensionTabProps) {
  const { dimension: rawDimension } = useParams<{ dimension: string }>()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()

  // Restore date filter from URL search params (preserves context from overview)
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
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  const handleRefresh = React.useCallback(() => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as string
        return !!(
          key &&
          typeof key === 'string' &&
          (key.includes('getPropertyBreakdown') ||
            key.includes('getEventsCount'))
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
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

  if (!isDimensionKey(rawDimension)) {
    return (
      <div className="p-8 text-center">
        <p className="text-sm text-muted-foreground">
          Unknown analytics dimension: {rawDimension}
        </p>
      </div>
    )
  }

  const dimension: DimensionKey = rawDimension

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
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      <DimensionList
        project={project}
        dimension={dimension}
        startDate={startDate}
        endDate={endDate}
        environment={selectedEnvironment}
        onBack={() => {
          const params = new URLSearchParams()
          params.set('filter', dateFilter.quickFilter)
          if (
            dateFilter.quickFilter === 'custom' &&
            dateFilter.dateRange?.from &&
            dateFilter.dateRange?.to
          ) {
            params.set('from', dateFilter.dateRange.from.toISOString())
            params.set('to', dateFilter.dateRange.to.toISOString())
          }
          const qs = params.toString()
          navigate(`/projects/${project.slug}/analytics${qs ? `?${qs}` : ''}`)
        }}
      />
    </div>
  )
}

// AI Agents Tab — full "view all" page for AI crawler traffic + crawled pages.
// Mirrors DimensionTab's URL/date-filter behaviour so the active range carries
// over from the overview card's "View all" link.
interface AiAgentsTabProps {
  project: ProjectResponse
  /** `overview` (default) = chart + cards; `tables` = the full "View all" page. */
  view?: 'overview' | 'tables'
}

function AiAgentsTab({ project, view = 'overview' }: AiAgentsTabProps) {
  const navigate = useNavigate()
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
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  const handleRefresh = React.useCallback(() => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as string
        return !!(
          key &&
          typeof key === 'string' &&
          (key.includes('getAiAgentBreakdown') ||
            key.includes('getAiPageBreakdown'))
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
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

  // Serialise the active date filter so navigation preserves the window.
  const dateQs = React.useCallback(() => {
    const params = new URLSearchParams()
    params.set('filter', dateFilter.quickFilter)
    if (
      dateFilter.quickFilter === 'custom' &&
      dateFilter.dateRange?.from &&
      dateFilter.dateRange?.to
    ) {
      params.set('from', dateFilter.dateRange.from.toISOString())
      params.set('to', dateFilter.dateRange.to.toISOString())
    }
    const qs = params.toString()
    return qs ? `?${qs}` : ''
  }, [dateFilter])

  const goBack = React.useCallback(() => {
    // From the tables ("View all") page, Back returns to the AI overview;
    // from the overview it returns to the main analytics overview.
    const dest =
      view === 'tables'
        ? `/projects/${project.slug}/analytics/ai-agents`
        : `/projects/${project.slug}/analytics`
    navigate(`${dest}${dateQs()}`)
  }, [navigate, project.slug, view, dateQs])

  const onViewAll = React.useCallback(() => {
    navigate(`/projects/${project.slug}/analytics/ai-agents/all${dateQs()}`)
  }, [navigate, project.slug, dateQs])

  const onViewAllProviders = React.useCallback(() => {
    const qs = dateQs()
    const sep = qs ? '&' : '?'
    navigate(
      `/projects/${project.slug}/analytics/ai-agents/all${qs}${sep}group=provider`
    )
  }, [navigate, project.slug, dateQs])

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
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      <AiAgentsDetail
        project={project}
        startDate={startDate}
        endDate={endDate}
        environment={selectedEnvironment}
        onBack={goBack}
        view={view}
        onViewAll={view === 'overview' ? onViewAll : undefined}
        onViewAllProviders={view === 'overview' ? onViewAllProviders : undefined}
        defaultGroupBy={
          searchParams.get('group') === 'provider' ? 'provider' : 'agent'
        }
      />
    </div>
  )
}

// Segment Visitors Tab — paginated visitors for one dimension value (e.g.
// "browsers / Chrome"). Mirrors DimensionTab's URL/date-filter behaviour so
// quick filters and custom ranges propagate cleanly.
interface SegmentVisitorsTabProps {
  project: ProjectResponse
}

function SegmentVisitorsTab({ project }: SegmentVisitorsTabProps) {
  const { dimension: rawDimension, value: rawValue } = useParams<{
    dimension: string
    value: string
  }>()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()

  const [dateFilter, setDateFilter] = React.useState<AnalyticsDateFilter>(() => {
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
  })
  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
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
        const key = query.queryKey[0] as string
        return !!(
          key &&
          typeof key === 'string' &&
          key.includes('getVisitors')
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  if (!isDimensionKey(rawDimension) || !rawValue) {
    return (
      <div className="p-8 text-center">
        <p className="text-sm text-muted-foreground">
          Unknown analytics segment: {rawDimension}/{rawValue}
        </p>
      </div>
    )
  }

  const dimension: DimensionKey = rawDimension
  const value = decodeURIComponent(rawValue)

  if (!segmentSupportsVisitors(dimension)) {
    return (
      <div className="p-8 text-center">
        <p className="text-sm text-muted-foreground">
          {dimension} segments can&apos;t be drilled into visitors.
        </p>
      </div>
    )
  }

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
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      <SegmentVisitors
        project={project}
        dimension={dimension}
        value={value}
        startDate={startDate}
        endDate={endDate}
        environment={selectedEnvironment}
        onBack={() => {
          const params = new URLSearchParams()
          params.set('filter', dateFilter.quickFilter)
          if (
            dateFilter.quickFilter === 'custom' &&
            dateFilter.dateRange?.from &&
            dateFilter.dateRange?.to
          ) {
            params.set('from', dateFilter.dateRange.from.toISOString())
            params.set('to', dateFilter.dateRange.to.toISOString())
          }
          const qs = params.toString()
          navigate(
            `/projects/${project.slug}/analytics/dimensions/${dimension}${qs ? `?${qs}` : ''}`
          )
        }}
      />
    </div>
  )
}

// Session Replays Tab Component
interface SessionReplaysTabProps {
  project: ProjectResponse
}

function SessionReplaysTab({ project }: SessionReplaysTabProps) {
  const [dateFilter, setDateFilter] = React.useState<AnalyticsDateFilter>({
    quickFilter: '24hours',
    dateRange: undefined,
  })
  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  const handleRefresh = React.useCallback(() => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as string
        return !!(
          key &&
          typeof key === 'string' &&
          (key.includes('visitors') || key.includes('sessions'))
        )
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  return (
    <div className="space-y-6">
      {/* Date Filter and Environment Selector */}
      <AnalyticsFilters
        project={project}
        activeFilter={dateFilter.quickFilter}
        dateRange={dateFilter.dateRange}
        selectedEnvironment={selectedEnvironment}
        onFilterChange={(filter) =>
          setDateFilter((prev) => ({ ...prev, quickFilter: filter }))
        }
        onDateRangeChange={(range) =>
          setDateFilter((prev) => ({
            quickFilter: range ? 'custom' : prev.quickFilter,
            dateRange: range,
          }))
        }
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      {/* Session Replays Component */}
      <SessionReplays
        project={project}
        startDate={startDate}
        endDate={endDate}
      />
    </div>
  )
}

// Journey Tab Component
interface JourneyTabProps {
  project: ProjectResponse
}

function JourneyTab({ project }: JourneyTabProps) {
  const [dateFilter, setDateFilter] = React.useState<AnalyticsDateFilter>({
    quickFilter: '24hours',
    dateRange: undefined,
  })
  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const queryClient = useQueryClient()

  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  const handleRefresh = React.useCallback(() => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as string
        return !!(key && typeof key === 'string' && key.includes('getPageFlow'))
      },
    })
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [queryClient])

  return (
    <div className="space-y-6">
      <AnalyticsFilters
        project={project}
        activeFilter={dateFilter.quickFilter}
        dateRange={dateFilter.dateRange}
        selectedEnvironment={selectedEnvironment}
        onFilterChange={(filter) =>
          setDateFilter((prev) => ({ ...prev, quickFilter: filter }))
        }
        onDateRangeChange={(range) =>
          setDateFilter((prev) => ({
            quickFilter: range ? 'custom' : prev.quickFilter,
            dateRange: range,
          }))
        }
        onEnvironmentChange={setSelectedEnvironment}
        onRefresh={handleRefresh}
        isRefreshing={isRefreshing}
      />

      <PageFlow
        project={project}
        startDate={startDate}
        endDate={endDate}
        environment={selectedEnvironment}
      />
    </div>
  )
}

interface ProjectAnalyticsProps {
  project: ProjectResponse
}

export function ProjectAnalytics({ project }: ProjectAnalyticsProps) {
  return (
    <Routes>
      <Route index element={<ProjectAnalyticsOverview project={project} />} />
      <Route path="requests/*" element={<RequestLogs project={project} />} />
      <Route path="funnels/*" element={<FunnelAnalytics project={project} />} />
      <Route
        path="live-visitors"
        element={<LiveVisitors project={project} />}
      />
      <Route
        path="visitors/*"
        element={<VisitorAnalytics project={project} />}
      />
      <Route path="pages" element={<PagesTab project={project} />} />
      <Route
        path="events/:eventName"
        element={<EventDetailTab project={project} />}
      />
      <Route
        path="dimensions/:dimension"
        element={<DimensionTab project={project} />}
      />
      <Route path="ai-agents" element={<AiAgentsTab project={project} />} />
      <Route
        path="ai-agents/all"
        element={<AiAgentsTab project={project} view="tables" />}
      />
      <Route
        path="segments/:dimension/:value"
        element={<SegmentVisitorsTab project={project} />}
      />
      <Route path="replays" element={<SessionReplaysTab project={project} />} />
      <Route path="setup" element={<AnalyticsSetup project={project} />} />
      <Route path="live" element={<LiveGlobePage project={project} />} />
      <Route path="globe" element={<VisitorGlobePage project={project} />} />
      <Route path="journey" element={<JourneyTab project={project} />} />
    </Routes>
  )
}
interface ProjectAnalyticsOverviewProps {
  project: ProjectResponse
}
function ProjectAnalyticsOverview({ project }: ProjectAnalyticsOverviewProps) {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()

  // Restore date filter from URL search params (enables browser back/forward)
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

  // Sync date filter to URL search params
  const updateDateFilter = React.useCallback(
    (next: AnalyticsDateFilter) => {
      setDateFilter(next)
      const params = new URLSearchParams()
      params.set('filter', next.quickFilter)
      if (
        next.quickFilter === 'custom' &&
        next.dateRange?.from &&
        next.dateRange?.to
      ) {
        params.set('from', next.dateRange.from.toISOString())
        params.set('to', next.dateRange.to.toISOString())
      }
      setSearchParams(params, { replace: false })
    },
    [setSearchParams]
  )

  // Listen for popstate (browser back/forward) and restore date filter
  React.useEffect(() => {
    const filter = searchParams.get('filter') as QuickFilter | null
    const from = searchParams.get('from')
    const to = searchParams.get('to')

    if (filter === 'custom' && from && to) {
      setDateFilter({
        quickFilter: 'custom',
        dateRange: { from: new Date(from), to: new Date(to) },
      })
    } else if (filter && QUICK_FILTERS.some((f) => f.value === filter)) {
      setDateFilter({ quickFilter: filter, dateRange: undefined })
    }
  }, [searchParams])

  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const [showSetupOverride] = React.useState(false)
  const queryClient = useQueryClient()
  const { startDate, endDate } = getDateRangeFromFilter(dateFilter)

  // Chart zoom handler — sets a custom date range from drag selection
  const handleChartZoom = React.useCallback(
    (from: Date, to: Date) => {
      updateDateFilter({
        quickFilter: 'custom',
        dateRange: { from, to },
      })
    },
    [updateDateFilter]
  )

  // Check if we have any analytics data using the new endpoint
  const hasAnalyticsEventsQuery = useQuery({
    ...hasAnalyticsEventsOptions({
      path: {
        project_id: project.id,
      },
    }),
    enabled: true,
  })

  const hasNoData = React.useMemo(() => {
    if (hasAnalyticsEventsQuery.isLoading || !hasAnalyticsEventsQuery.data)
      return false
    return !hasAnalyticsEventsQuery.data.has_events
  }, [hasAnalyticsEventsQuery.data, hasAnalyticsEventsQuery.isLoading])

  React.useEffect(() => {
    if (hasNoData && !showSetupOverride) {
      navigate(`/projects/${project.slug}/analytics/setup`)
    }
  }, [hasNoData, showSetupOverride, project.slug, navigate])

  const handleRefresh = React.useCallback(async () => {
    setIsRefreshing(true)
    try {
      // Invalidate all analytics queries for this project
      await queryClient.invalidateQueries({
        predicate: (query) => {
          const queryKey = query.queryKey
          return (
            Array.isArray(queryKey) &&
            queryKey.some(
              (key) =>
                typeof key === 'object' &&
                key &&
                'query' in key &&
                typeof key.query === 'object' &&
                key.query &&
                'project_id_or_slug' in key.query &&
                key.query.project_id_or_slug === project.slug
            )
          )
        },
      })
      // Also invalidate the hasAnalyticsEvents query specifically
      await hasAnalyticsEventsQuery.refetch()
    } finally {
      setIsRefreshing(false)
    }
  }, [queryClient, project.slug, hasAnalyticsEventsQuery])

  return (
    <>
      <div className="space-y-6">
        {hasNoData && (
          <Card className="border-yellow-200 bg-yellow-50 dark:border-yellow-800 dark:bg-yellow-950/50">
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Info className="h-4 w-4 text-yellow-600 dark:text-yellow-400" />
                  <p className="text-sm font-medium text-yellow-900 dark:text-yellow-100">
                    No analytics data detected yet
                  </p>
                </div>
                <Button
                  variant="link"
                  size="sm"
                  className="text-yellow-600 dark:text-yellow-400"
                  onClick={() =>
                    navigate(`/projects/${project.slug}/analytics/setup`)
                  }
                >
                  View Setup Instructions
                </Button>
              </div>
            </CardHeader>
          </Card>
        )}

        <div className="flex flex-col gap-6">
          <AnalyticsFilters
            project={project}
            activeFilter={dateFilter.quickFilter}
            dateRange={dateFilter.dateRange}
            selectedEnvironment={selectedEnvironment}
            onFilterChange={(filter) =>
              updateDateFilter({
                ...dateFilter,
                quickFilter: filter,
                dateRange: undefined,
              })
            }
            onDateRangeChange={(range) =>
              updateDateFilter({
                quickFilter: range ? 'custom' : dateFilter.quickFilter,
                dateRange: range,
              })
            }
            onEnvironmentChange={setSelectedEnvironment}
            onRefresh={handleRefresh}
            isRefreshing={isRefreshing}
          />

          {/* Analytics Metrics */}
          <AnalyticsMetrics
            project={project}
            startDate={startDate}
            endDate={endDate}
            environment={selectedEnvironment}
          />
          <div className="relative">
            {dateFilter.quickFilter === 'custom' && dateFilter.dateRange && (
              <div className="flex justify-end mb-2">
                <Button
                  variant="outline"
                  size="sm"
                  className="h-7 text-xs gap-1.5"
                  onClick={() =>
                    updateDateFilter({
                      quickFilter: '30days',
                      dateRange: undefined,
                    })
                  }
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                  Reset zoom
                </Button>
              </div>
            )}
            <VisitorChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
              onZoom={handleChartZoom}
            />
          </div>
          {/* Globe link */}
          <Card
            className="cursor-pointer hover:bg-accent/50 transition-colors"
            onClick={() =>
              navigate(`/projects/${project.slug}/analytics/globe`)
            }
          >
            <CardContent className="flex items-center justify-between gap-3 py-3 sm:py-4">
              <div className="flex items-center gap-3 min-w-0">
                <Globe className="h-5 w-5 text-muted-foreground shrink-0" />
                <div className="min-w-0">
                  <p className="font-medium text-sm">Visitor Globe</p>
                  <p className="text-xs text-muted-foreground hidden sm:block">
                    See where your visitors are coming from on an interactive 3D
                    globe
                  </p>
                </div>
              </div>
              <Button variant="outline" size="sm" className="shrink-0">
                View Globe
              </Button>
            </CardContent>
          </Card>

          {/* Analytics Charts */}
          <div className="grid grid-cols-1 gap-4 sm:gap-6 md:grid-cols-2">
            <PagesChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <LocationsChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <ReferrersChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <BrowsersChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <AiAgentsChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <OperatingSystemChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <DevicesChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <ChannelsChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <LanguagesChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <UTMCampaignsChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
            <EventsChart
              project={project}
              startDate={startDate}
              endDate={endDate}
              environment={selectedEnvironment}
            />
          </div>
        </div>
      </div>
    </>
  )
}
interface ChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

function EventsChart({ project, startDate, endDate, environment }: ChartProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const { data, isLoading, error } = useQuery({
    ...getEventsCountOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = React.useMemo(() => {
    if (!data) return []

    return data
      .sort((a, b) => b.count - a.count)
      .slice(0, 5)
      .map((item) => ({
        event: item.event_name,
        count: item.count,
        percentage: item.percentage.toFixed(1),
      }))
  }, [data])

  const totalEvents = data?.length ?? 0
  const hasMore = totalEvents > chartData.length

  const handleViewAll = React.useCallback(() => {
    const params = new URLSearchParams()
    const filter = searchParams.get('filter')
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    if (filter) params.set('filter', filter)
    if (from) params.set('from', from)
    if (to) params.set('to', to)
    const qs = params.toString()
    navigate(
      `/projects/${project.slug}/analytics/dimensions/events${qs ? `?${qs}` : ''}`
    )
  }, [navigate, project.slug, searchParams])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Events</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          {!isLoading && !error && chartData.length > 0 && (
            <Button
              variant="ghost"
              size="sm"
              className="text-xs"
              onClick={handleViewAll}
            >
              View all
            </Button>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {['e1', 'e2', 'e3', 'e4', 'e5'].map((key) => (
                <div key={key} className="flex items-center justify-between">
                  <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                  <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
                </div>
              ))}
            </div>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load events data
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !chartData.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Event</TableHead>
                <TableHead className="text-right">Total</TableHead>
                <TableHead className="text-right">Percentage</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {chartData.map((item) => (
                <TableRow
                  key={item.event}
                  className="cursor-pointer hover:bg-muted/50"
                  onClick={(e) => {
                    // Carry the active date filter into the event detail page.
                    const params = new URLSearchParams()
                    const filter = searchParams.get('filter')
                    const from = searchParams.get('from')
                    const to = searchParams.get('to')
                    if (filter) params.set('filter', filter)
                    if (from) params.set('from', from)
                    if (to) params.set('to', to)
                    const qs = params.toString()
                    const url = `/projects/${project.slug}/analytics/events/${encodeURIComponent(item.event)}${qs ? `?${qs}` : ''}`
                    if (e.metaKey || e.ctrlKey) {
                      window.open(url, '_blank')
                    } else {
                      navigate(url)
                    }
                  }}
                >
                  <TableCell className="font-medium">{item.event}</TableCell>
                  <TableCell className="text-right">
                    {item.count.toLocaleString()}
                  </TableCell>
                  <TableCell className="text-right">
                    {item.percentage}%
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
      {!isLoading && !error && chartData.length > 0 && (
        <CardFooter className="flex-col gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {chartData.length} of {totalEvents.toLocaleString()}{' '}
            event{totalEvents === 1 ? '' : 's'} by count
            {hasMore ? ' — click "View all" to see the rest' : ''}
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

// Funnel Analytics Component
function FunnelAnalytics({ project }: ProjectAnalyticsProps) {
  const location = useLocation()

  // Check for create funnel path
  if (location.pathname.includes('/funnels/create')) {
    return <CreateFunnel project={project} />
  }

  // Check for edit funnel path
  if (
    location.pathname.includes('/funnels/') &&
    location.pathname.includes('/edit')
  ) {
    const funnelId = parseInt(location.pathname.split('/funnels/')[1])
    return <EditFunnel project={project} funnelId={funnelId} />
  }

  const isDetailView =
    location.pathname.includes('/funnels/') &&
    location.pathname.split('/funnels/')[1]

  if (isDetailView) {
    const funnelId = parseInt(location.pathname.split('/funnels/')[1])
    return <FunnelDetail project={project} funnelId={funnelId} />
  }

  return <FunnelManagement project={project} />
}

// Analytics Setup Component
function AnalyticsSetup({ project }: ProjectAnalyticsProps) {
  const [selectedFramework, setSelectedFramework] = React.useState('nextjs-app')
  const [selectedPackageManager, setSelectedPackageManager] =
    React.useState('npm')

  // Package manager commands
  const getInstallCommand = (basePackage: string) => {
    switch (selectedPackageManager) {
      case 'npm':
        return `npm install ${basePackage}`
      case 'yarn':
        return `yarn add ${basePackage}`
      case 'pnpm':
        return `pnpm add ${basePackage}`
      case 'bun':
        return `bun add ${basePackage}`
      default:
        return `npm install ${basePackage}`
    }
  }

  // Framework icons as inline SVG components
  const NextJsIcon = () => (
    <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
      <path d="M11.572 0c-.176 0-.31.001-.358.007a19.76 19.76 0 0 1-.364.033C7.443.346 4.25 2.185 2.228 5.012a11.875 11.875 0 0 0-2.119 5.243c-.096.659-.108.854-.108 1.747s.012 1.089.108 1.748c.652 4.506 3.86 8.292 8.209 9.695.779.25 1.6.422 2.534.525.363.04 1.935.04 2.299 0 1.611-.178 2.977-.577 4.323-1.264.207-.106.247-.134.219-.158-.02-.013-.9-1.193-1.955-2.62l-1.919-2.592-2.404-3.558a338.739 338.739 0 0 0-2.422-3.556c-.009-.002-.018 1.579-.023 3.51-.007 3.38-.01 3.515-.052 3.595a.426.426 0 0 1-.206.214c-.075.037-.14.044-.495.044H7.81l-.108-.068a.438.438 0 0 1-.157-.171l-.05-.106.006-4.703.007-4.705.072-.092a.645.645 0 0 1 .174-.143c.096-.047.134-.051.54-.051.478 0 .558.018.682.154.035.038 1.337 1.999 2.895 4.361a10760.433 10760.433 0 0 0 4.735 7.17l1.9 2.879.096-.063a12.317 12.317 0 0 0 2.466-2.163 11.944 11.944 0 0 0 2.824-6.134c.096-.66.108-.854.108-1.748 0-.893-.012-1.088-.108-1.747-.652-4.506-3.859-8.292-8.208-9.695a12.597 12.597 0 0 0-2.499-.523A33.119 33.119 0 0 0 11.573 0zm4.069 7.217c.347 0 .408.005.486.047a.473.473 0 0 1 .237.277c.018.06.023 1.365.018 4.304l-.006 4.218-.744-1.14-.746-1.14v-3.066c0-1.982.01-3.097.023-3.15a.478.478 0 0 1 .233-.296c.096-.05.13-.054.5-.054z" />
    </svg>
  )

  const ReactIcon = () => (
    <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
      <path d="M14.23 12.004a2.236 2.236 0 0 1-2.235 2.236 2.236 2.236 0 0 1-2.236-2.236 2.236 2.236 0 0 1 2.235-2.236 2.236 2.236 0 0 1 2.236 2.236zm2.648-10.69c-1.346 0-3.107.96-4.888 2.622-1.78-1.653-3.542-2.602-4.887-2.602-.41 0-.783.093-1.106.278-1.375.793-1.683 3.264-.973 6.365C1.98 8.917 0 10.42 0 12.004c0 1.59 1.99 3.097 5.043 4.03-.704 3.113-.39 5.588.988 6.38.32.187.69.275 1.102.275 1.345 0 3.107-.96 4.888-2.624 1.78 1.654 3.542 2.603 4.887 2.603.41 0 .783-.09 1.106-.275 1.374-.792 1.683-3.263.973-6.365C22.02 15.096 24 13.59 24 12.004c0-1.59-1.99-3.097-5.043-4.032.704-3.11.39-5.587-.988-6.38a2.167 2.167 0 0 0-1.092-.278zm-.005 1.09v.006c.225 0 .406.044.558.127.666.382.955 1.835.73 3.704-.054.46-.142.945-.25 1.44a23.476 23.476 0 0 0-3.107-.534A23.892 23.892 0 0 0 12.769 4.62c1.055-.98 2.047-1.524 2.86-1.524zM6.21 2.396c.154 0 .32.02.52.075.654.228 1.23.915 1.704 1.836a19.807 19.807 0 0 0-2.04 2.452 20.004 20.004 0 0 0-3.098.536c-.112-.49-.195-.964-.254-1.42-.23-1.868.054-3.32.714-3.707.19-.09.4-.127.563-.127z" />
    </svg>
  )

  const ViteIcon = () => (
    <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
      <path d="m8.286 10.578.512-8.657a.306.306 0 0 1 .247-.282L17.377.006a.306.306 0 0 1 .353.385l-1.558 5.403a.306.306 0 0 0 .352.385l2.388-.46a.306.306 0 0 1 .332.438l-6.79 13.55-.123.19a.294.294 0 0 1-.252.14c-.177 0-.35-.152-.305-.369l1.095-5.301a.306.306 0 0 0-.388-.355l-1.433.435a.306.306 0 0 1-.389-.354l.69-3.375a.306.306 0 0 0-.37-.36l-2.32.536a.306.306 0 0 1-.374-.316z" />
    </svg>
  )

  const frameworks = [
    {
      id: 'nextjs-app',
      name: 'Next.js',
      category: 'Next.js',
      icon: NextJsIcon,
      description: 'App Router (13+)',
      packageName: '@temps-sdk/react-analytics',
      setupCode: `// app/layout.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';
import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Your App",
  description: "Your app description",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>
        <TempsAnalyticsProvider basePath="/api/_temps">
          {children}
        </TempsAnalyticsProvider>
      </body>
    </html>
  );
}`,
      envExample: `# .env.local
TEMPS_API_KEY=your_api_key_here # Get this from your Temps dashboard
NEXT_PUBLIC_PROJECT_SLUG=${project.slug}
NEXT_PUBLIC_TEMPS_API_URL=https://your-temps-instance.com`,
    },
    {
      id: 'nextjs-pages',
      name: 'Next.js',
      category: 'Next.js',
      icon: NextJsIcon,
      description: 'Pages Router',
      packageName: '@temps-sdk/react-analytics',
      setupCode: `// pages/_app.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';
import type { AppProps } from 'next/app';

function MyApp({ Component, pageProps }: AppProps) {
  return (
    <TempsAnalyticsProvider basePath="/api/_temps">
      <Component {...pageProps} />
    </TempsAnalyticsProvider>
  );
}

export default MyApp;`,
      apiRouteCode: `// pages/api/_temps/[...path].ts
import type { NextApiRequest, NextApiResponse } from 'next';

export default async function handler(
  req: NextApiRequest,
  res: NextApiResponse
) {
  if (req.method === 'POST') {
    // Forward analytics events to Temps API
    const response = await fetch(\`\${process.env.NEXT_PUBLIC_TEMPS_API_URL}/api/analytics/\${process.env.NEXT_PUBLIC_PROJECT_SLUG}/events\`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': \`Bearer \${process.env.TEMPS_API_KEY}\`,
      },
      body: JSON.stringify(req.body),
    });

    if (!response.ok) {
      return res.status(response.status).json({ error: 'Failed to send analytics' });
    }

    return res.status(200).json({ success: true });
  }

  if (req.method === 'GET') {
    return res.status(200).json({ status: 'ok' });
  }

  return res.status(405).json({ error: 'Method not allowed' });
}`,
      envExample: `# .env.local
TEMPS_API_KEY=your_api_key_here # Get this from your Temps dashboard
NEXT_PUBLIC_PROJECT_SLUG=${project.slug}
NEXT_PUBLIC_TEMPS_API_URL=https://your-temps-instance.com`,
    },
    {
      id: 'vite',
      name: 'Vite',
      category: 'React',
      icon: ViteIcon,
      description: 'React + Vite',
      packageName: '@temps-sdk/react-analytics',
      setupCode: `// src/main.tsx
import React from 'react'
import ReactDOM from 'react-dom/client'
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics'
import App from './App'

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <TempsAnalyticsProvider>
      <App />
    </TempsAnalyticsProvider>
  </React.StrictMode>,
)`,
    },
    {
      id: 'react',
      name: 'React',
      category: 'React',
      icon: ReactIcon,
      description: 'Create React App',
      packageName: '@temps-sdk/react-analytics',
      setupCode: `// src/index.tsx
import React from 'react';
import ReactDOM from 'react-dom/client';
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';
import App from './App';

const root = ReactDOM.createRoot(
  document.getElementById('root') as HTMLElement
);

root.render(
  <React.StrictMode>
    <TempsAnalyticsProvider>
      <App />
    </TempsAnalyticsProvider>
  </React.StrictMode>
);`,
    },
    {
      id: 'remix',
      name: 'Remix',
      category: 'React',
      icon: ReactIcon,
      description: 'Remix Framework',
      installCommand: 'npm install @temps-sdk/react-analytics',
      setupCode: `// app/root.tsx
import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics';
import {
  Links,
  Meta,
  Outlet,
  Scripts,
  ScrollRestoration,
} from "@remix-run/react";

export default function App() {
  return (
    <html lang="en">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <Meta />
        <Links />
      </head>
      <body>
        <TempsAnalyticsProvider basePath="/api/_temps">
          <Outlet />
          <ScrollRestoration />
          <Scripts />
        </TempsAnalyticsProvider>
      </body>
    </html>
  );
}`,
    },
  ]

  const selectedFrameworkData =
    frameworks.find((f) => f.id === selectedFramework) || frameworks[0]

  // Generate AI prompt for coding agents based on the selected framework
  const getAnalyticsAiPrompt = () => {
    const fw = selectedFrameworkData
    return `Add Temps analytics to my ${fw.name}${fw.description ? ` (${fw.description})` : ''} application.

## Using an AI coding CLI? Install the Temps skill

Works with Claude Code, OpenCode, Codex, and any other CLI that supports the skills format. The \`add-react-analytics\` skill has the canonical, framework-specific instructions and stays up to date.

\`\`\`bash
# Install the skill (one-time)
npx skills add https://github.com/gotempsh/temps --skill add-react-analytics

# Then invoke it in your CLI:
/add-react-analytics
\`\`\`

If the skill is already installed, just run \`/add-react-analytics\` and skip the manual steps below.

## Installation

\`\`\`bash
${getInstallCommand(fw.packageName || '@temps-sdk/react-analytics')}
\`\`\`

## Setup

Wrap the app with the TempsAnalyticsProvider. Here is the framework-specific setup:

\`\`\`tsx
${fw.setupCode}
\`\`\`
${fw.envExample ? `\n## Environment Variables\n\n\`\`\`bash\n${fw.envExample}\n\`\`\`` : ''}
${fw.apiRouteCode ? `\n## API Route (for proxying analytics events)\n\n\`\`\`typescript\n${fw.apiRouteCode}\n\`\`\`` : ''}

## Provider Configuration

The TempsAnalyticsProvider accepts these options:

\`\`\`tsx
<TempsAnalyticsProvider
  basePath="/api/_temps"
  autoTrack={{
    pageviews: true,       // Auto-track page views
    pageLeave: true,       // Track time on page
    speedAnalytics: true,  // Track Web Vitals (LCP, FCP, CLS, TTFB, INP)
    engagement: true,      // Track engagement
    engagementInterval: 30000,
  }}
  debug={process.env.NODE_ENV === 'development'}
>
  {children}
</TempsAnalyticsProvider>
\`\`\`

## Available Hooks

| Hook | Purpose |
|------|---------|
| \`useTrackEvent\` | Track custom events |
| \`useAnalytics\` | Access analytics context, identify users |
| \`useScrollVisibility\` | Track element visibility on scroll |
| \`usePageLeave\` | Track page leave and time on page |
| \`useEngagementTracking\` | Heartbeat engagement monitoring |
| \`useSpeedAnalytics\` | Web Vitals (LCP, FCP, CLS, TTFB, INP) |
| \`useTrackPageview\` | Manual page view tracking |

## Track Custom Events

\`\`\`tsx
'use client';
import { useTrackEvent } from '@temps-sdk/react-analytics';

function MyComponent() {
  const trackEvent = useTrackEvent();

  const handleClick = () => {
    trackEvent('button_click', {
      button_id: 'subscribe',
      plan: 'premium'
    });
  };

  return <button onClick={handleClick}>Subscribe</button>;
}
\`\`\`

## Identify Users

\`\`\`tsx
'use client';
import { useAnalytics } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';

function UserProfile({ user }) {
  const { identify } = useAnalytics();

  useEffect(() => {
    if (user) {
      identify(user.id, {
        email: user.email,
        name: user.name,
        plan: user.subscription?.plan
      });
    }
  }, [user, identify]);

  return <div>Profile</div>;
}
\`\`\`

## Verification

After implementation:
1. Check browser DevTools Network tab for \`/api/_temps\` requests
2. Verify events appear in the Temps dashboard
3. Confirm Web Vitals are being captured`
  }

  const [wizardStep, setWizardStep] = React.useState<WizardStepId>('framework')
  const [celebrate, setCelebrate] = React.useState(false)
  const navigate = useNavigate()

  const { data: hasEventsData } = useQuery({
    ...hasAnalyticsEventsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id && wizardStep === 'waiting',
    refetchInterval: wizardStep === 'waiting' ? 2000 : false,
    refetchOnWindowFocus: false,
  })

  React.useEffect(() => {
    if (wizardStep === 'waiting' && hasEventsData?.has_events && !celebrate) {
      setCelebrate(true)
      const timer = setTimeout(() => {
        navigate(`/projects/${project.slug}/analytics`)
      }, 1600)
      return () => clearTimeout(timer)
    }
  }, [wizardStep, hasEventsData?.has_events, celebrate, navigate, project.slug])

  const steps = [
    { id: 'framework' as WizardStepId, label: 'Framework' },
    { id: 'install' as WizardStepId, label: 'Install' },
    { id: 'waiting' as WizardStepId, label: 'Verify' },
  ]

  return (
    <SetupWizardShell
      title="Install analytics"
      description="Pick your framework, drop in the provider, and we'll wait for your first event."
      currentStep={wizardStep}
      steps={steps}
      celebrate={celebrate}
    >
      {wizardStep === 'framework' && (
        <div className="space-y-4">
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            {frameworks.map((framework) => {
              const FrameworkIcon = framework.icon
              const isSelected = selectedFramework === framework.id
              return (
                <button
                  key={framework.id}
                  type="button"
                  onClick={() => setSelectedFramework(framework.id)}
                  className={cn(
                    'flex items-center gap-3 rounded-lg border bg-card p-4 text-left transition-all hover:border-primary/60 hover:bg-accent/40',
                    isSelected &&
                      'border-primary bg-primary/5 ring-2 ring-primary/20'
                  )}
                  aria-pressed={isSelected}
                >
                  <div className="rounded-md bg-muted p-2 text-foreground">
                    <FrameworkIcon />
                  </div>
                  <div className="flex-1 min-w-0">
                    <p className="font-medium leading-none">{framework.name}</p>
                    {framework.description && (
                      <p className="mt-1 text-xs text-muted-foreground">
                        {framework.description}
                      </p>
                    )}
                  </div>
                  {isSelected && (
                    <Check className="size-4 shrink-0 text-primary" />
                  )}
                </button>
              )
            })}
          </div>
          <div className="flex justify-end">
            <Button onClick={() => setWizardStep('install')}>
              Continue
              <ArrowRight className="ml-2 size-4" />
            </Button>
          </div>
        </div>
      )}

      {wizardStep === 'install' && (
        <div className="space-y-6">
          <div className="flex items-center justify-between gap-3 rounded-lg border bg-card p-4">
            <div className="flex items-center gap-3 min-w-0">
              <div className="rounded-md bg-muted p-2">
                {(selectedFrameworkData?.icon || ReactIcon)()}
              </div>
              <div className="min-w-0">
                <p className="font-medium leading-none">
                  {selectedFrameworkData.name}
                </p>
                {selectedFrameworkData.description && (
                  <p className="mt-1 text-xs text-muted-foreground">
                    {selectedFrameworkData.description}
                  </p>
                )}
              </div>
            </div>
            <CopyButton
              value={getAnalyticsAiPrompt()}
              className="shrink-0 rounded-md border border-border px-3 py-1.5 text-xs font-medium"
            >
              Copy AI prompt
            </CopyButton>
          </div>

          <div className="rounded-lg border border-primary/30 bg-primary/5 p-4">
            <div className="flex items-start gap-3">
              <Terminal className="mt-0.5 size-4 shrink-0 text-primary" />
              <div className="min-w-0 space-y-2">
                <div>
                  <p className="text-sm font-medium">
                    Using an AI coding CLI? Run the Temps skill.
                  </p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Works with Claude Code, OpenCode, Codex, and any CLI that
                    supports skills. The{' '}
                    <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                      add-react-analytics
                    </code>{' '}
                    skill auto-detects your framework and wires the provider,
                    env vars, and proxy route for you.
                  </p>
                </div>
                <div className="space-y-1.5">
                  <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                    1. Install the skill (one-time)
                  </p>
                  <CodeBlock
                    language="bash"
                    code={`npx skills add https://github.com/gotempsh/temps --skill add-react-analytics`}
                    showCopy
                  />
                </div>
                <div className="space-y-1.5">
                  <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                    2. Invoke it in your CLI
                  </p>
                  <CodeBlock
                    language="bash"
                    code={`/add-react-analytics`}
                    showCopy
                  />
                </div>
              </div>
            </div>
          </div>

          {selectedFrameworkData.packageName && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Terminal className="size-4 text-muted-foreground" />
                <h3 className="text-sm font-medium">1. Install the SDK</h3>
              </div>
              <div className="flex gap-1 rounded-lg border border-border bg-muted p-1 w-fit">
                {['npm', 'yarn', 'pnpm', 'bun'].map((pm) => {
                  const isSelected = selectedPackageManager === pm
                  return (
                    <button
                      key={pm}
                      type="button"
                      onClick={() => setSelectedPackageManager(pm)}
                      className={cn(
                        'rounded-md px-3 py-1 text-xs font-medium transition-colors',
                        isSelected
                          ? 'bg-background text-foreground shadow-sm'
                          : 'text-muted-foreground hover:text-foreground'
                      )}
                      aria-pressed={isSelected}
                    >
                      {pm}
                    </button>
                  )
                })}
              </div>
              <CodeBlock
                language="bash"
                code={getInstallCommand(selectedFrameworkData.packageName)}
                showCopy
              />
            </div>
          )}

          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <FileCode className="size-4 text-muted-foreground" />
              <h3 className="text-sm font-medium">
                {selectedFrameworkData.packageName ? '2' : '1'}. Wrap your app
                with the provider
              </h3>
            </div>
            <CodeBlock
              language={
                selectedFrameworkData.id.includes('next')
                  ? 'typescript'
                  : 'javascript'
              }
              code={selectedFrameworkData.setupCode}
              showCopy
            />
          </div>

          {selectedFrameworkData.envExample && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <FileCode className="size-4 text-muted-foreground" />
                <h3 className="text-sm font-medium">
                  3. Environment variables
                </h3>
              </div>
              <CodeBlock
                language="bash"
                code={selectedFrameworkData.envExample}
                showCopy
              />
            </div>
          )}

          <div className="flex items-center justify-between gap-3 pt-2">
            <Button variant="ghost" onClick={() => setWizardStep('framework')}>
              <ArrowLeft className="mr-2 size-4" />
              Back
            </Button>
            <Button onClick={() => setWizardStep('waiting')}>
              I've installed it — start listening
              <ArrowRight className="ml-2 size-4" />
            </Button>
          </div>
        </div>
      )}

      {wizardStep === 'waiting' && (
        <div className="space-y-6">
          <div className="flex flex-col items-center justify-center gap-4 rounded-xl border bg-card px-6 py-12 text-center">
            {hasEventsData?.has_events ? (
              <>
                <div className="flex size-14 items-center justify-center rounded-full bg-emerald-500/10">
                  <Check className="size-7 text-emerald-500" strokeWidth={3} />
                </div>
                <div className="space-y-1">
                  <h3 className="text-lg font-semibold">
                    First event received
                  </h3>
                  <p className="text-sm text-muted-foreground">
                    Taking you to your analytics dashboard…
                  </p>
                </div>
              </>
            ) : (
              <>
                <div className="relative flex size-14 items-center justify-center">
                  <span className="absolute inline-flex size-full animate-ping rounded-full bg-primary/20" />
                  <span className="absolute inline-flex size-10 animate-ping rounded-full bg-primary/30 [animation-delay:200ms]" />
                  <span className="relative inline-flex size-4 rounded-full bg-primary" />
                </div>
                <div className="space-y-1">
                  <h3 className="text-lg font-semibold">
                    Waiting for your first event…
                  </h3>
                  <p className="text-sm text-muted-foreground">
                    Deploy your app or run it locally. We'll auto-redirect as
                    soon as an event arrives.
                  </p>
                </div>
                <div className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Loader2 className="size-3 animate-spin" />
                  Polling every 2s
                </div>
              </>
            )}
          </div>

          {!hasEventsData?.has_events && (
            <details className="rounded-lg border bg-card p-4 text-sm">
              <summary className="cursor-pointer font-medium">
                Not seeing anything? Double-check the setup.
              </summary>
              <ol className="mt-3 space-y-2 text-muted-foreground">
                <li>1. Confirm the provider wraps your app's root.</li>
                <li>
                  2. Check DevTools → Network for <code>/api/_temps</code>{' '}
                  requests.
                </li>
                <li>3. Reload a page to trigger a pageview.</li>
              </ol>
            </details>
          )}

          <div className="flex items-center gap-3">
            <Button
              variant="ghost"
              onClick={() => setWizardStep('install')}
              disabled={celebrate}
            >
              <ArrowLeft className="mr-2 size-4" />
              Back to instructions
            </Button>
          </div>
        </div>
      )}
    </SetupWizardShell>
  )
}
