import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Calendar } from '@/components/ui/calendar'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Skeleton } from '@/components/ui/skeleton'
import { getFunnelMetricsOptions } from '@/api/client/@tanstack/react-query.gen'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { format, subDays } from 'date-fns'
import { ArrowLeft, Calendar as CalendarIcon } from 'lucide-react'
import * as React from 'react'
import { DateRange } from 'react-day-picker'
import { useNavigate } from 'react-router-dom'
import { FunnelVisualization } from './FunnelVisualization'

interface FunnelDetailProps {
  project: ProjectResponse
  funnelId: number
}

export function FunnelDetail({ project, funnelId }: FunnelDetailProps) {
  const navigate = useNavigate()
  const [dateRange, setDateRange] = React.useState<DateRange | undefined>({
    from: subDays(new Date(), 30),
    to: new Date(),
  })

  const {
    data: metrics,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getFunnelMetricsOptions({
      path: {
        project_id: project.id,
        funnel_id: funnelId,
      },
      query: {
        start_date: dateRange?.from?.toISOString(),
        end_date: dateRange?.to?.toISOString(),
      },
    }),
    enabled: !!dateRange?.from && !!dateRange?.to,
  })

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="icon" onClick={() => navigate(-1)}>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="text-2xl font-semibold">
              {metrics?.funnel_name || 'Funnel Details'}
            </h2>
            <p className="text-muted-foreground">
              Conversion funnel visualization
            </p>
          </div>
        </div>
        <Popover>
          <PopoverTrigger asChild>
            <Button
              variant="outline"
              className={cn(
                'justify-start text-left font-normal',
                !dateRange && 'text-muted-foreground'
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
                <span>Pick a date range</span>
              )}
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-auto p-0" align="end">
            <Calendar
              initialFocus
              mode="range"
              defaultMonth={dateRange?.from}
              selected={dateRange}
              onSelect={setDateRange}
              numberOfMonths={2}
              disabled={(date) => date > new Date()}
            />
          </PopoverContent>
        </Popover>
      </div>

      {isLoading ? (
        <div className="space-y-6">
          {/* Summary Cards Skeleton */}
          <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
            {Array.from({ length: 4 }).map((_, i) => (
              <Card key={i}>
                <CardHeader className="pb-2">
                  <Skeleton className="h-3 w-24" />
                </CardHeader>
                <CardContent>
                  <Skeleton className="h-8 w-16" />
                </CardContent>
              </Card>
            ))}
          </div>

          {/* Funnel Visualization Skeleton */}
          <Card>
            <CardHeader>
              <Skeleton className="h-6 w-48 mb-2" />
              <Skeleton className="h-4 w-64" />
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Entry Point Skeleton */}
              <div className="flex items-center gap-4">
                <Skeleton className="w-10 h-10 rounded-full" />
                <div className="flex-1">
                  <div className="bg-muted rounded-lg p-4">
                    <div className="flex items-center justify-between">
                      <div className="space-y-2">
                        <Skeleton className="h-4 w-24" />
                        <Skeleton className="h-3 w-32" />
                      </div>
                      <div className="space-y-2 text-right">
                        <Skeleton className="h-6 w-16" />
                        <Skeleton className="h-3 w-12" />
                      </div>
                    </div>
                  </div>
                </div>
              </div>

              {/* Steps Skeleton */}
              {Array.from({ length: 3 }).map((_, i) => (
                <div key={i} className="space-y-2">
                  <div className="flex items-center gap-2 ml-14">
                    <Skeleton className="h-4 w-4" />
                    <Skeleton className="h-3 w-24" />
                  </div>
                  <div className="flex items-center gap-4">
                    <Skeleton className="w-10 h-10 rounded-full" />
                    <div className="flex-1">
                      <div
                        className="bg-muted/50 rounded-lg p-4"
                        style={{ width: `${85 - i * 20}%` }}
                      >
                        <div className="flex items-center justify-between">
                          <div className="space-y-2">
                            <Skeleton className="h-4 w-20" />
                            <Skeleton className="h-3 w-24" />
                          </div>
                          <div className="space-y-2 text-right">
                            <Skeleton className="h-5 w-14" />
                            <Skeleton className="h-3 w-20" />
                          </div>
                        </div>
                      </div>
                    </div>
                  </div>
                </div>
              ))}
            </CardContent>
          </Card>
        </div>
      ) : error ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <p className="text-muted-foreground mb-2">
              Failed to load funnel metrics
            </p>
            <Button variant="outline" onClick={() => refetch()}>
              Try again
            </Button>
          </CardContent>
        </Card>
      ) : !metrics ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <p className="text-muted-foreground">
              No data available for the selected period
            </p>
          </CardContent>
        </Card>
      ) : (
        <Card>
          <CardHeader>
            <CardTitle>Funnel Analysis</CardTitle>
            <CardDescription>
              User progression and conversion metrics
            </CardDescription>
          </CardHeader>
          <CardContent>
            <FunnelVisualization
              totalEntries={metrics.total_entries}
              stepConversions={metrics.step_conversions}
              conversionRate={metrics.overall_conversion_rate}
              averageCompletionTime={metrics.average_completion_time_seconds}
            />
          </CardContent>
        </Card>
      )}
    </div>
  )
}
