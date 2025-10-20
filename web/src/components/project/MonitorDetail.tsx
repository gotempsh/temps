import { ProjectResponse, StatusBucket } from '@/api/client'
import {
  getBucketedStatusOptions,
  getCurrentMonitorStatusOptions,
  getMonitorOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { Calendar } from '@/components/ui/calendar'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { useQuery } from '@tanstack/react-query'
import {
  Activity,
  AlertCircle,
  ArrowLeft,
  Calendar as CalendarIcon,
  Clock,
  TrendingUp,
} from 'lucide-react'
import { useMemo, useState, useRef } from 'react'
import { Link, useParams } from 'react-router-dom'
import { format, subDays } from 'date-fns'
import { DateRange } from 'react-day-picker'
import { cn } from '@/lib/utils'

interface MonitorDetailProps {
  project: ProjectResponse
}

interface BucketItemProps {
  bucket: StatusBucket
  isOpen: boolean
  onOpenChange: (open: boolean) => void
}

function BucketItem({ bucket, isOpen, onOpenChange }: BucketItemProps) {
  const timeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined)

  const handleMouseEnter = () => {
    clearTimeout(timeoutRef.current)
    timeoutRef.current = setTimeout(() => onOpenChange(true), 200)
  }

  const handleMouseLeave = () => {
    clearTimeout(timeoutRef.current)
    timeoutRef.current = setTimeout(() => onOpenChange(false), 200)
  }

  return (
    <Popover
      open={isOpen}
      onOpenChange={(open) => !open && onOpenChange(false)}
    >
      <PopoverTrigger
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
        asChild
      >
        <div
          className={`flex-1 rounded-sm transition-opacity hover:opacity-80 cursor-pointer ${
            bucket.status === 'operational'
              ? 'bg-green-500'
              : bucket.status === 'major_outage'
                ? 'bg-red-500'
                : bucket.status === 'degraded'
                  ? 'bg-yellow-500'
                  : 'bg-gray-300'
          }`}
        />
      </PopoverTrigger>
      <PopoverContent
        className="w-72 p-3"
        align="center"
        side="bottom"
        sideOffset={8}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
      >
        <div className="space-y-2">
          <div className="pb-2 border-b">
            <h4 className="font-semibold text-sm">Status Details</h4>
            <p className="text-xs text-muted-foreground mt-1">
              {new Date(bucket.bucket_start).toLocaleString()}
            </p>
          </div>
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">Status</span>
              <Badge
                variant={
                  bucket.status === 'operational'
                    ? 'default'
                    : bucket.status === 'major_outage'
                      ? 'destructive'
                      : 'secondary'
                }
                className="text-xs"
              >
                {bucket.status === 'major_outage'
                  ? 'Major Outage'
                  : bucket.status}
              </Badge>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">
                Avg Response Time
              </span>
              <span className="text-xs font-medium">
                {bucket.avg_response_time_ms?.toFixed(0) ?? 'N/A'}ms
              </span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">
                Total Checks
              </span>
              <span className="text-xs font-medium">
                {bucket.total_checks ?? 0}
              </span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">
                Successful Checks
              </span>
              <span className="text-xs font-medium">
                {bucket.operational_count ?? 0}
              </span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">
                Failed Checks
              </span>
              <span className="text-xs font-medium">
                {bucket.down_count ?? 0}
              </span>
            </div>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  )
}

type QuickFilter =
  | 'today'
  | 'yesterday'
  | '24hours'
  | '7days'
  | '30days'
  | 'custom'

const QUICK_FILTERS = [
  { label: 'Today', value: 'today' as const },
  { label: 'Yesterday', value: 'yesterday' as const },
  { label: 'Last 24 hours', value: '24hours' as const },
  { label: 'Last 7 Days', value: '7days' as const },
  { label: 'Last 30 Days', value: '30days' as const },
]

export function MonitorDetail({ project }: MonitorDetailProps) {
  const { monitorId } = useParams()
  const [interval, setInterval] = useState<'5min' | 'hourly' | 'daily'>(
    'hourly'
  )
  const [activeFilter, setActiveFilter] = useState<QuickFilter>('24hours')
  const [dateRange, setDateRange] = useState<DateRange | undefined>(undefined)
  const [hoveredBucket, setHoveredBucket] = useState<number | null>(null)

  // Memoize start and end dates to prevent unnecessary refetches
  const { startDate, endDate } = useMemo(() => {
    const now = new Date()
    if (activeFilter === 'custom' && dateRange) {
      return {
        startDate: dateRange.from,
        endDate: dateRange.to,
      }
    }

    switch (activeFilter) {
      case 'today': {
        const todayStart = new Date(now)
        todayStart.setHours(0, 0, 0, 0)
        const todayEnd = new Date(now)
        todayEnd.setHours(23, 59, 59, 999)
        return {
          startDate: todayStart,
          endDate: todayEnd,
        }
      }
      case 'yesterday': {
        const yesterdayStart = new Date(now)
        yesterdayStart.setDate(yesterdayStart.getDate() - 1)
        yesterdayStart.setHours(0, 0, 0, 0)
        const yesterdayEnd = new Date(now)
        yesterdayEnd.setDate(yesterdayEnd.getDate() - 1)
        yesterdayEnd.setHours(23, 59, 59, 999)
        return {
          startDate: yesterdayStart,
          endDate: yesterdayEnd,
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
        return {
          startDate: subDays(now, 7),
          endDate: now,
        }
      }
      case '30days': {
        return {
          startDate: subDays(now, 30),
          endDate: now,
        }
      }
      default: {
        return {
          startDate: subDays(now, 7),
          endDate: now,
        }
      }
    }
  }, [activeFilter, dateRange])

  const {
    data: monitor,
    isLoading: isLoadingMonitor,
    error: monitorError,
    refetch: refetchMonitor,
  } = useQuery({
    ...getMonitorOptions({
      path: {
        monitor_id: parseInt(monitorId || '0'),
      },
    }),
    enabled: !!monitorId,
  })
  const {
    data: currentMonitorStatus,
    isLoading: isLoadingCurrentMonitorStatus,
    error: currentMonitorStatusError,
    refetch: refetchCurrentMonitorStatus,
  } = useQuery({
    ...getCurrentMonitorStatusOptions({
      path: {
        monitor_id: parseInt(monitorId || '0'),
      },
      query: {
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
      },
    }),
  })

  const {
    data: statusData,
    isLoading: isLoadingStatus,
    error: statusError,
    refetch: refetchStatus,
  } = useQuery({
    ...getBucketedStatusOptions({
      path: {
        monitor_id: parseInt(monitorId || '0'),
      },
      query: {
        interval,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
      },
    }),
    enabled: !!monitorId && !!startDate && !!endDate,
    refetchInterval: 30000, // Refresh every 30 seconds
  })
  // Calculate uptime stats from status data (filtered by date range)
  const uptimePercentage = useMemo(
    () => currentMonitorStatus?.uptime_percentage ?? 0,
    [currentMonitorStatus]
  )
  const avgResponseTime = useMemo(
    () => currentMonitorStatus?.avg_response_time_ms ?? 0,
    [currentMonitorStatus]
  )
  const currentStatus = useMemo(
    () => currentMonitorStatus?.current_status ?? 'unknown',
    [currentMonitorStatus]
  )

  if (monitorError || currentMonitorStatusError) {
    return (
      <div className="p-6">
        <ErrorAlert
          title="Failed to load monitor"
          description={
            monitorError instanceof Error
              ? monitorError.message
              : 'An unexpected error occurred'
          }
          retry={() => {
            refetchMonitor()
            refetchCurrentMonitorStatus()
          }}
        />
      </div>
    )
  }

  if (isLoadingMonitor || isLoadingCurrentMonitorStatus) {
    return (
      <div className="space-y-6">
        <div className="flex items-center gap-4">
          <Skeleton className="h-10 w-10 rounded-md" />
          <div className="space-y-2">
            <Skeleton className="h-6 w-48" />
            <Skeleton className="h-4 w-32" />
          </div>
        </div>
        <div className="grid gap-4 md:grid-cols-3">
          {Array.from({ length: 3 }).map((_, i) => (
            <Card key={i}>
              <CardHeader>
                <Skeleton className="h-4 w-24" />
              </CardHeader>
              <CardContent>
                <Skeleton className="h-8 w-20" />
              </CardContent>
            </Card>
          ))}
        </div>
      </div>
    )
  }

  if (!monitor) {
    return (
      <div className="p-6">
        <ErrorAlert
          title="Monitor not found"
          description="The monitor you're looking for doesn't exist."
        />
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Link to={`/projects/${project.slug}/monitors`}>
            <Button variant="outline" size="icon">
              <ArrowLeft className="h-4 w-4" />
            </Button>
          </Link>
          <div>
            <h2 className="text-2xl font-bold tracking-tight">
              {monitor.name}
            </h2>
            <p className="text-muted-foreground">
              Monitor status and performance metrics
            </p>
          </div>
        </div>
        <Badge variant={monitor.is_active ? 'default' : 'secondary'}>
          {monitor.is_active ? 'Active' : 'Inactive'}
        </Badge>
      </div>

      {/* Date Range Filter */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-end gap-2">
        <div className="flex items-center gap-2">
          <div className="hidden sm:flex gap-1">
            {QUICK_FILTERS.map((filter) => (
              <Button
                key={filter.value}
                variant={activeFilter === filter.value ? 'default' : 'outline'}
                size="sm"
                onClick={() => setActiveFilter(filter.value)}
              >
                {filter.label}
              </Button>
            ))}
          </div>
          <div className="sm:hidden">
            <Select
              value={activeFilter}
              onValueChange={(v) => setActiveFilter(v as QuickFilter)}
            >
              <SelectTrigger className="w-[140px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {QUICK_FILTERS.map((filter) => (
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
                onSelect={(range) => {
                  setDateRange(range)
                  if (range) {
                    setActiveFilter('custom')
                  }
                }}
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

      {/* Stats Cards */}
      <div className="grid gap-4 md:grid-cols-3">
        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">
              Current Status
            </CardTitle>
            <Activity className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <div
                className={`h-3 w-3 rounded-full ${
                  currentStatus === 'operational'
                    ? 'bg-green-500'
                    : currentStatus === 'major_outage'
                      ? 'bg-red-500'
                      : currentStatus === 'degraded'
                        ? 'bg-yellow-500'
                        : 'bg-gray-400'
                }`}
              />
              <div className="text-2xl font-bold capitalize">
                {currentStatus === 'major_outage'
                  ? 'Major Outage'
                  : currentStatus}
              </div>
            </div>
            <p className="text-xs text-muted-foreground mt-1">
              Last checked{' '}
              {new Date(
                currentMonitorStatus?.last_check_at || monitor.created_at
              ).toLocaleString()}
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">Uptime</CardTitle>
            <TrendingUp className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            {isLoadingStatus ? (
              <Skeleton className="h-8 w-20" />
            ) : (
              <>
                <div className="text-2xl font-bold">
                  {uptimePercentage.toFixed(2)}%
                </div>
                <p className="text-xs text-muted-foreground mt-1">
                  {startDate && endDate
                    ? `${format(startDate, 'MMM dd')} - ${format(endDate, 'MMM dd')}`
                    : 'Select a date range'}
                </p>
              </>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
            <CardTitle className="text-sm font-medium">
              Avg Response Time
            </CardTitle>
            <Clock className="h-4 w-4 text-muted-foreground" />
          </CardHeader>
          <CardContent>
            {isLoadingStatus ? (
              <Skeleton className="h-8 w-20" />
            ) : (
              <>
                <div className="text-2xl font-bold">
                  {avgResponseTime.toFixed(0)}ms
                </div>
                <p className="text-xs text-muted-foreground mt-1">
                  Average over selected period
                </p>
              </>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Status Timeline */}
      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <div>
            <CardTitle>Status Timeline</CardTitle>
            <CardDescription>
              Historical uptime and performance data
            </CardDescription>
          </div>
          <Select
            value={interval}
            onValueChange={(value: '5min' | 'hourly' | 'daily') =>
              setInterval(value)
            }
          >
            <SelectTrigger className="w-32">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="minute">Minute</SelectItem>
              <SelectItem value="5min">5 Minutes</SelectItem>
              <SelectItem value="hourly">Hourly</SelectItem>
              <SelectItem value="daily">Daily</SelectItem>
            </SelectContent>
          </Select>
        </CardHeader>
        <CardContent>
          {statusError ? (
            <ErrorAlert
              title="Failed to load status data"
              description={
                statusError instanceof Error
                  ? statusError.message
                  : 'An unexpected error occurred'
              }
              retry={() => refetchStatus()}
            />
          ) : isLoadingStatus ? (
            <div className="space-y-4">
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-8 w-full" />
            </div>
          ) : statusData?.buckets && statusData.buckets.length > 0 ? (
            <div className="space-y-4">
              {/* Status bar visualization */}
              <div className="flex gap-1 h-12">
                {statusData.buckets.map((bucket, idx) => (
                  <BucketItem
                    key={idx}
                    bucket={bucket}
                    isOpen={hoveredBucket === idx}
                    onOpenChange={(open) => setHoveredBucket(open ? idx : null)}
                  />
                ))}
              </div>

              {/* Legend */}
              <div className="flex items-center gap-4 text-sm">
                <div className="flex items-center gap-2">
                  <div className="h-3 w-3 rounded-sm bg-green-500" />
                  <span>Operational</span>
                </div>
                <div className="flex items-center gap-2">
                  <div className="h-3 w-3 rounded-sm bg-yellow-500" />
                  <span>Degraded</span>
                </div>
                <div className="flex items-center gap-2">
                  <div className="h-3 w-3 rounded-sm bg-red-500" />
                  <span>Major Outage</span>
                </div>
                <div className="flex items-center gap-2">
                  <div className="h-3 w-3 rounded-sm bg-gray-300" />
                  <span>Unknown</span>
                </div>
              </div>
            </div>
          ) : (
            <div className="flex flex-col items-center justify-center py-8 text-center">
              <AlertCircle className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-sm text-muted-foreground">
                No status data available yet. Check back after the monitor has
                been running for a while.
              </p>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Configuration Details */}
      <Card>
        <CardHeader>
          <CardTitle>Configuration</CardTitle>
          <CardDescription>Monitor settings and details</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2 md:col-span-2">
              <p className="text-sm font-medium text-muted-foreground">URL</p>
              <p className="text-sm font-mono break-all">
                {monitor.monitor_url}
              </p>
            </div>
            <div className="space-y-2">
              <p className="text-sm font-medium text-muted-foreground">
                Monitor Type
              </p>
              <Badge variant="outline">{monitor.monitor_type}</Badge>
            </div>
            <div className="space-y-2">
              <p className="text-sm font-medium text-muted-foreground">
                Check Interval
              </p>
              <p className="text-sm">
                {monitor.check_interval_seconds} seconds
              </p>
            </div>
            <div className="space-y-2">
              <p className="text-sm font-medium text-muted-foreground">
                Project ID
              </p>
              <p className="text-sm">{monitor.project_id}</p>
            </div>
            <div className="space-y-2">
              <p className="text-sm font-medium text-muted-foreground">
                Created
              </p>
              <p className="text-sm">
                {new Date(monitor.created_at).toLocaleString()}
              </p>
            </div>
            {monitor.environment_id && (
              <div className="space-y-2">
                <p className="text-sm font-medium text-muted-foreground">
                  Environment ID
                </p>
                <p className="text-sm">{monitor.environment_id}</p>
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
