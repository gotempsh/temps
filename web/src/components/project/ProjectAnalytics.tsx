import {
  getEnvironmentsOptions,
  getEventsCountOptions,
  getHourlyVisitsOptions,
  hasAnalyticsEventsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import {
  AnalyticsMetrics,
  BrowsersChart,
  LocationsChart,
  PagesChart,
} from '@/components/analytics/overview'
import { Pages } from '@/components/analytics/Pages'
import { SessionReplays } from '@/components/analytics/SessionReplays'
import { FunnelDetail } from '@/components/funnel/FunnelDetail'
import { FunnelManagement } from '@/components/funnel/FunnelManagement'
import { LiveVisitorsList } from '@/components/visitors/LiveVisitorsList'
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
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import VisitorAnalytics from '@/components/visitors/VisitorAnalytics'
import { cn } from '@/lib/utils'
import { CreateFunnel } from '@/pages/CreateFunnel'
import { EditFunnel } from '@/pages/EditFunnel'
import RequestLogs from '@/pages/RequestLogs'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { format, subDays } from 'date-fns'
import {
  Calendar as CalendarIcon,
  Code2,
  FileCode,
  Info,
  RefreshCw,
  Terminal,
} from 'lucide-react'
import * as React from 'react'
import { DateRange } from 'react-day-picker'
import { Route, Routes, useLocation, useNavigate } from 'react-router-dom'

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
}

export function VisitorChart({
  project,
  startDate,
  endDate,
  environment,
}: VisitorChartProps) {
  const [aggregationLevel, setAggregationLevel] = React.useState<
    'events' | 'sessions' | 'visitors'
  >('visitors')

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
      <div className="flex items-center justify-between">
        <CardTitle>{getChartTitle()}</CardTitle>
        <div className="flex gap-2">
          <Badge
            variant={aggregationLevel === 'events' ? 'default' : 'outline'}
            className="cursor-pointer"
            onClick={() => setAggregationLevel('events')}
          >
            Events
          </Badge>
          <Badge
            variant={aggregationLevel === 'sessions' ? 'default' : 'outline'}
            className="cursor-pointer"
            onClick={() => setAggregationLevel('sessions')}
          >
            Sessions
          </Badge>
          <Badge
            variant={aggregationLevel === 'visitors' ? 'default' : 'outline'}
            className="cursor-pointer"
            onClick={() => setAggregationLevel('visitors')}
          >
            Visitors
          </Badge>
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
        <ChartContainer config={chartConfig2} className="h-[250px] w-full">
          <LineChart
            accessibilityLayer
            data={chartData}
            margin={{
              left: 12,
              right: 12,
              top: 12,
              bottom: 12,
            }}
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
      )}
    </div>
  )
}

const QUICK_FILTERS = [
  { label: 'Today', value: 'today' },
  { label: 'Yesterday', value: 'yesterday' },
  { label: 'Last 24 hours', value: '24hours' },
  { label: 'Last 7 Days', value: '7days' },
  { label: 'Last 30 Days', value: '30days' },
  { label: 'Custom', value: 'custom' },
] as const

type QuickFilter = (typeof QUICK_FILTERS)[number]['value']

interface AnalyticsFiltersProps {
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

function AnalyticsFilters({
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
                  'min-w-[140px]',
                  !dateRange?.from && 'text-muted-foreground'
                )}
              >
                <CalendarIcon className="mr-2 h-4 w-4" />
                {dateRange?.from ? (
                  dateRange.to ? (
                    <>
                      {format(dateRange.from, 'LLL dd, y')} -{' '}
                      {format(dateRange.to, 'LLL dd, y')}
                    </>
                  ) : (
                    format(dateRange.from, 'LLL dd, y')
                  )
                ) : (
                  <span>Custom range</span>
                )}
              </Button>
            </PopoverTrigger>
            <PopoverContent className="w-auto p-0" align="end">
              <Calendar
                initialFocus
                mode="range"
                defaultMonth={
                  new Date(new Date().setMonth(new Date().getMonth() - 1))
                }
                selected={dateRange}
                onSelect={onDateRangeChange}
                numberOfMonths={2}
                disabled={(date) => date > new Date()}
                toDate={new Date()}
                fromDate={
                  new Date(new Date().setMonth(new Date().getMonth() - 1))
                }
              />
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
  const [dateFilter, setDateFilter] = React.useState<AnalyticsDateFilter>({
    quickFilter: '24hours',
    dateRange: undefined,
  })
  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const queryClient = useQueryClient()

  const getDateRange = React.useCallback(() => {
    const now = new Date()
    if (dateFilter.quickFilter === 'custom' && dateFilter.dateRange) {
      return {
        startDate: dateFilter.dateRange.from,
        endDate: dateFilter.dateRange.to,
      }
    }

    switch (dateFilter.quickFilter) {
      case 'today':
        return {
          startDate: new Date(now.setHours(0, 0, 0, 0)),
          endDate: new Date(now.setHours(23, 59, 59, 999)),
        }
      case 'yesterday': {
        const yesterday = new Date(now)
        yesterday.setDate(yesterday.getDate() - 1)
        return {
          startDate: new Date(yesterday.setHours(0, 0, 0, 0)),
          endDate: new Date(yesterday.setHours(23, 59, 59, 999)),
        }
      }
      case '24hours': {
        const twentyFourHoursAgo = new Date(now)
        twentyFourHoursAgo.setHours(twentyFourHoursAgo.getHours() - 24)
        return {
          startDate: twentyFourHoursAgo,
          endDate: now,
        }
      }
      case '7days':
        return {
          startDate: subDays(now, 7),
          endDate: now,
        }
      case '30days':
        return {
          startDate: subDays(now, 30),
          endDate: now,
        }
      default:
        return {
          startDate: subDays(now, 7),
          endDate: now,
        }
    }
  }, [dateFilter])

  const { startDate, endDate } = getDateRange()

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

      {/* Pages Component */}
      <Pages
        project={project}
        startDate={startDate}
        endDate={endDate}
        environment={selectedEnvironment}
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

  const getDateRange = React.useCallback(() => {
    const now = new Date()
    if (dateFilter.quickFilter === 'custom' && dateFilter.dateRange) {
      return {
        startDate: dateFilter.dateRange.from,
        endDate: dateFilter.dateRange.to,
      }
    }

    switch (dateFilter.quickFilter) {
      case 'today':
        return {
          startDate: new Date(now.setHours(0, 0, 0, 0)),
          endDate: new Date(now.setHours(23, 59, 59, 999)),
        }
      case 'yesterday': {
        const yesterday = new Date(now)
        yesterday.setDate(yesterday.getDate() - 1)
        return {
          startDate: new Date(yesterday.setHours(0, 0, 0, 0)),
          endDate: new Date(yesterday.setHours(23, 59, 59, 999)),
        }
      }
      case '24hours': {
        const twentyFourHoursAgo = new Date(now)
        twentyFourHoursAgo.setHours(twentyFourHoursAgo.getHours() - 24)
        return {
          startDate: twentyFourHoursAgo,
          endDate: now,
        }
      }
      case '7days':
        return {
          startDate: subDays(now, 7),
          endDate: now,
        }
      case '30days':
        return {
          startDate: subDays(now, 30),
          endDate: now,
        }
      default:
        return {
          startDate: subDays(now, 7),
          endDate: now,
        }
    }
  }, [dateFilter])

  const { startDate, endDate } = getDateRange()

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

interface ProjectAnalyticsProps {
  project: ProjectResponse
}
interface AnalyticsDateFilter {
  quickFilter: QuickFilter
  dateRange: DateRange | undefined
}

export function ProjectAnalytics({ project }: ProjectAnalyticsProps) {
  return (
    <Routes>
      <Route index element={<ProjectAnalyticsOverview project={project} />} />
      <Route path="requests/*" element={<RequestLogs project={project} />} />
      <Route path="funnels/*" element={<FunnelAnalytics project={project} />} />
      <Route path="live-visitors" element={<LiveVisitors project={project} />} />
      <Route
        path="visitors/*"
        element={<VisitorAnalytics project={project} />}
      />
      <Route path="pages" element={<PagesTab project={project} />} />
      <Route path="replays" element={<SessionReplaysTab project={project} />} />
      <Route path="setup" element={<AnalyticsSetup project={project} />} />
    </Routes>
  )
}
interface ProjectAnalyticsOverviewProps {
  project: ProjectResponse
}
function ProjectAnalyticsOverview({ project }: ProjectAnalyticsOverviewProps) {
  const navigate = useNavigate()
  const [dateFilter, setDateFilter] = React.useState<AnalyticsDateFilter>({
    quickFilter: '24hours',
    dateRange: undefined,
  })
  const [selectedEnvironment, setSelectedEnvironment] = React.useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = React.useState(false)
  const [showSetupOverride] = React.useState(false)
  const queryClient = useQueryClient()
  const getDateRange = React.useCallback(() => {
    const now = new Date()

    if (dateFilter.quickFilter === 'custom' && dateFilter.dateRange) {
      return {
        startDate: dateFilter.dateRange.from,
        endDate: dateFilter.dateRange.to,
      }
    }

    switch (dateFilter.quickFilter) {
      case 'today':
        return {
          startDate: new Date(now.setHours(0, 0, 0, 0)),
          endDate: new Date(now.setHours(23, 59, 59, 999)),
        }
      case 'yesterday': {
        const yesterday = new Date(now)
        yesterday.setDate(yesterday.getDate() - 1)
        return {
          startDate: new Date(yesterday.setHours(0, 0, 0, 0)),
          endDate: new Date(yesterday.setHours(23, 59, 59, 999)),
        }
      }
      case '24hours': {
        const twentyFourHoursAgo = new Date(now)
        twentyFourHoursAgo.setHours(twentyFourHoursAgo.getHours() - 24)
        return {
          startDate: twentyFourHoursAgo,
          endDate: now,
        }
      }
      case '7days': {
        const sevenDaysAgo = new Date(now)
        sevenDaysAgo.setDate(sevenDaysAgo.getDate() - 7)
        return {
          startDate: new Date(sevenDaysAgo.setHours(0, 0, 0, 0)),
          endDate: new Date(now.setHours(23, 59, 59, 999)),
        }
      }
      case '30days':
      default: {
        const thirtyDaysAgo = new Date(now)
        thirtyDaysAgo.setDate(thirtyDaysAgo.getDate() - 30)
        return {
          startDate: new Date(thirtyDaysAgo.setHours(0, 0, 0, 0)),
          endDate: new Date(now.setHours(23, 59, 59, 999)),
        }
      }
    }
  }, [dateFilter])
  const { startDate, endDate } = getDateRange()

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

  // Auto-navigate to setup if no data detected
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
        {/* Show banner if there's no data */}
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

          {/* Analytics Metrics */}
          <AnalyticsMetrics
            project={project}
            startDate={startDate}
            endDate={endDate}
            environment={selectedEnvironment}
          />
          <VisitorChart
            project={project}
            startDate={startDate}
            endDate={endDate}
            environment={selectedEnvironment}
          />
          {/* Analytics Charts */}
          <div className="grid grid-cols-1 gap-6 sm:grid-cols-2 lg:grid-cols-2">
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
            <BrowsersChart
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
  const { data, isLoading, error } = useQuery({
    ...getEventsCountOptions({
      query: {
        project_id: project.id,
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

  return (
    <Card>
      <CardHeader>
        <CardTitle>Events</CardTitle>
        <CardDescription>
          {startDate && endDate
            ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
            : 'Select a date range'}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div key={i} className="flex items-center justify-between">
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
                <TableRow key={item.event}>
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
            Showing top {chartData.length} events by count
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
    return <EditFunnel project={project} />
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

  // Group frameworks by category
  const frameworksByCategory = frameworks.reduce(
    (acc, framework) => {
      if (!acc[framework.category]) acc[framework.category] = []
      acc[framework.category].push(framework)
      return acc
    },
    {} as Record<string, typeof frameworks>
  )

  return (
    <div className="space-y-6">
      {/* Header */}
      <Card>
        <CardHeader>
          <div className="flex items-start gap-3">
            <div className="rounded-lg bg-primary/10 p-2">
              <Code2 className="h-5 w-5 text-primary" />
            </div>
            <div className="space-y-1">
              <CardTitle>Analytics Setup Instructions</CardTitle>
              <CardDescription>
                Choose your framework and follow the instructions to integrate
                analytics
              </CardDescription>
            </div>
          </div>
        </CardHeader>
      </Card>

      {/* Framework Selection Tabs */}
      <Card>
        <CardHeader>
          <CardTitle>Select Your Framework</CardTitle>
        </CardHeader>
        <CardContent>
          <Tabs value={selectedFramework} onValueChange={setSelectedFramework}>
            <div className="w-full overflow-x-auto">
              <TabsList className="w-full justify-start h-auto p-1 flex-wrap">
                {Object.entries(frameworksByCategory).map(
                  ([category, categoryFrameworks]) => (
                    <div
                      key={category}
                      className="flex items-center gap-1 mr-4 last:mr-0"
                    >
                      <span className="text-xs text-muted-foreground px-2">
                        {category}:
                      </span>
                      {categoryFrameworks.map((framework) => {
                        const FrameworkIcon = framework.icon
                        return (
                          <TabsTrigger
                            key={framework.id}
                            value={framework.id}
                            className="data-[state=active]:bg-primary data-[state=active]:text-primary-foreground"
                          >
                            <div className="flex items-center gap-2">
                              <FrameworkIcon />
                              <span className="hidden sm:inline">
                                {framework.name}
                              </span>
                              {framework.description && (
                                <span className="hidden lg:inline text-xs opacity-70">
                                  ({framework.description})
                                </span>
                              )}
                            </div>
                          </TabsTrigger>
                        )
                      })}
                    </div>
                  )
                )}
              </TabsList>
            </div>
          </Tabs>
        </CardContent>
      </Card>

      {/* Selected Framework Instructions */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-3">
            <div className="rounded-lg bg-muted p-2">
              {(selectedFrameworkData?.icon || ReactIcon)()}
            </div>
            <div>
              <CardTitle className="text-lg">
                {selectedFrameworkData.name}
                {selectedFrameworkData.description && (
                  <span className="ml-2 text-sm font-normal text-muted-foreground">
                    {selectedFrameworkData.description}
                  </span>
                )}
              </CardTitle>
              <CardDescription>
                Follow these steps to add analytics to your{' '}
                {selectedFrameworkData.name} application
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-6">
          {/* Step 1: Install */}
          {selectedFrameworkData.packageName && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                  1
                </span>
                <h4 className="font-medium">Install the SDK</h4>
              </div>
              <div className="relative ml-8 space-y-3">
                <div className="flex items-center gap-2">
                  <Terminal className="h-4 w-4 text-muted-foreground" />
                  <span className="text-sm text-muted-foreground">
                    Choose your package manager:
                  </span>
                </div>

                {/* Package Manager Tabs */}
                <div className="flex gap-1 p-1 bg-muted rounded-lg w-fit border border-border">
                  {['npm', 'yarn', 'pnpm', 'bun'].map((pm) => {
                    const isSelected = selectedPackageManager === pm
                    return (
                      <Button
                        key={pm}
                        variant="ghost"
                        size="sm"
                        className={cn(
                          'px-3 py-1 h-7 text-xs transition-colors font-normal shadow-none',
                          isSelected
                            ? 'bg-foreground text-background border border-border shadow-md font-medium'
                            : 'hover:bg-accent hover:text-accent-foreground'
                        )}
                        style={
                          isSelected
                            ? {
                                boxShadow: '0 2px 8px 0 rgba(0,0,0,0.10)',
                                backgroundColor: 'var(--foreground)',
                                color: 'var(--background)',
                              }
                            : undefined
                        }
                        onClick={() => setSelectedPackageManager(pm)}
                        aria-pressed={isSelected}
                      >
                        {pm}
                      </Button>
                    )
                  })}
                </div>

                <CodeBlock
                  language="bash"
                  code={getInstallCommand(selectedFrameworkData.packageName!)}
                  showCopy
                />
              </div>
            </div>
          )}

          {/* Step 2: Add Provider */}
          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                {selectedFrameworkData.packageName ? '2' : '1'}
              </span>
              <h4 className="font-medium">Add the Analytics Provider</h4>
            </div>
            <div className="relative ml-8">
              <div className="flex items-center gap-2 mb-2">
                <FileCode className="h-4 w-4 text-muted-foreground" />
                <span className="text-sm text-muted-foreground">
                  Wrap your app with the provider:
                </span>
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
          </div>
        </CardContent>
      </Card>

      {/* Additional Configuration */}
      <Card>
        <CardHeader>
          <CardTitle>Usage Examples</CardTitle>
          <CardDescription>
            Learn how to use the analytics SDK in your application
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-6">
            {/* Custom Events Example */}
            <div className="space-y-3">
              <h4 className="font-medium">Track Custom Events</h4>
              <p className="text-sm text-muted-foreground">
                Use the useAnalytics hook to track custom events with metadata:
              </p>
              <CodeBlock
                language="javascript"
                code={`import { useAnalytics } from '@temps-sdk/react-analytics';

function MyComponent() {
  const { track } = useAnalytics();

  const handleClick = () => {
    // Track a custom event with properties
    track('button_click', {
      button_id: 'subscribe',
      page: '/pricing',
      plan: 'premium'
    });
  };

  return (
    <button onClick={handleClick}>
      Subscribe Now
    </button>
  );
}`}
              />
            </div>

            {/* Identify Users Example */}
            <div className="space-y-3">
              <h4 className="font-medium">Identify Users</h4>
              <p className="text-sm text-muted-foreground">
                Associate analytics with specific users:
              </p>
              <CodeBlock
                language="javascript"
                code={`import { useAnalytics } from '@temps-sdk/react-analytics';
import { useEffect } from 'react';

function UserProfile({ user }) {
  const { identify } = useAnalytics();

  useEffect(() => {
    if (user) {
      // Identify the user with their details
      identify(user.id, {
        email: user.email,
        name: user.name,
        plan: user.subscription.plan
      });
    }
  }, [user]);

  return <div>Profile content</div>;
}`}
              />
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Verification Steps */}
      <Card>
        <CardHeader>
          <CardTitle>Verify Your Installation</CardTitle>
          <CardDescription>
            Follow these steps to ensure analytics is working correctly
          </CardDescription>
        </CardHeader>
        <CardContent>
          <ol className="space-y-4">
            <li className="flex gap-3">
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full border-2 border-primary text-xs font-medium">
                1
              </span>
              <div className="space-y-1">
                <p className="font-medium">Deploy your changes</p>
                <p className="text-sm text-muted-foreground">
                  Push your code to staging or production environment
                </p>
              </div>
            </li>
            <li className="flex gap-3">
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full border-2 border-primary text-xs font-medium">
                2
              </span>
              <div className="space-y-1">
                <p className="font-medium">Visit your application</p>
                <p className="text-sm text-muted-foreground">
                  Navigate through a few pages to generate some traffic
                </p>
              </div>
            </li>
            <li className="flex gap-3">
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full border-2 border-primary text-xs font-medium">
                3
              </span>
              <div className="space-y-1">
                <p className="font-medium">Check the Analytics Dashboard</p>
                <p className="text-sm text-muted-foreground">
                  Return to the Overview tab to see your data in real-time
                </p>
              </div>
            </li>
            <li className="flex gap-3">
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full border-2 border-primary text-xs font-medium">
                4
              </span>
              <div className="space-y-1">
                <p className="font-medium">Debug if needed</p>
                <p className="text-sm text-muted-foreground">
                  Open browser console and look for any analytics errors
                </p>
              </div>
            </li>
          </ol>
        </CardContent>
        <CardFooter className="flex gap-2">
          <Button
            variant="outline"
            onClick={() =>
              window.open('https://docs.temps.sh/analytics', '_blank')
            }
          >
            View Documentation
          </Button>
          <Button
            variant="outline"
            onClick={() =>
              window.open('https://github.com/gotempsh/temps/issues', '_blank')
            }
          >
            Report an Issue
          </Button>
        </CardFooter>
      </Card>
    </div>
  )
}
