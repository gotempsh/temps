import { getErrorTimeSeriesOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import React, { useMemo, useState } from 'react'
import { Line, LineChart, XAxis, YAxis } from 'recharts'

interface ErrorTimeSeriesChartProps {
  project: ProjectResponse
  startDate: Date
  endDate: Date
}

const chartConfig = {
  count: {
    label: 'Errors',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

export function ErrorTimeSeriesChart({
  project,
  startDate,
  endDate,
}: ErrorTimeSeriesChartProps) {
  // Calculate time range in hours to determine appropriate bucket
  const timeRangeHours = useMemo(() => {
    return (endDate.getTime() - startDate.getTime()) / (1000 * 60 * 60)
  }, [startDate, endDate])

  // Auto-determine bucket based on time range, but allow manual override
  const defaultBucket = useMemo(() => {
    if (timeRangeHours <= 1) return '5m' // Last hour: 5 minutes
    if (timeRangeHours <= 24) return '1h' // Last 24 hours: 1 hour
    return '1d' // 7+ days: 1 day
  }, [timeRangeHours])

  const [selectedBucket, setSelectedBucket] = useState<'5m' | '1h' | '1d'>(
    defaultBucket
  )

  // Update selected bucket when time range changes
  React.useEffect(() => {
    setSelectedBucket(defaultBucket)
  }, [defaultBucket])

  const { data, isLoading, error } = useQuery({
    ...getErrorTimeSeriesOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_time: startDate.toISOString(),
        end_time: endDate.toISOString(),
        bucket: selectedBucket,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = useMemo(() => {
    if (!data || data.length === 0) return []

    // Format date based on bucket size
    const dateFormat =
      selectedBucket === '5m'
        ? 'HH:mm' // 5 minutes: show time only
        : selectedBucket === '1h'
          ? 'MMM dd HH:mm' // 1 hour: show date and time
          : 'MMM dd' // 1 day: show date only

    return data.map((point) => ({
      timestamp: point.timestamp,
      date: format(new Date(point.timestamp), dateFormat),
      count: point.count,
    }))
  }, [data, selectedBucket])

  const totalErrors = useMemo(() => {
    if (!data) return 0
    return data.reduce((sum, point) => sum + point.count, 0)
  }, [data])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Error Time Series</CardTitle>
            <CardDescription>
              {format(startDate, 'LLL dd, y')} - {format(endDate, 'LLL dd, y')}
            </CardDescription>
          </div>
          <div className="flex gap-2">
            {timeRangeHours <= 1 && (
              <Button
                variant={selectedBucket === '5m' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setSelectedBucket('5m')}
              >
                5 Min
              </Button>
            )}
            {timeRangeHours <= 24 && (
              <Button
                variant={selectedBucket === '1h' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setSelectedBucket('1h')}
              >
                Hourly
              </Button>
            )}
            <Button
              variant={selectedBucket === '1d' ? 'default' : 'outline'}
              size="sm"
              onClick={() => setSelectedBucket('1d')}
            >
              Daily
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <Skeleton className="h-[400px] w-full" />
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load error time series
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !chartData.length || !data || data.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No error data available for the selected period
            </p>
          </div>
        ) : (
          <div>
            <div className="mb-4">
              <div className="text-2xl font-bold">
                {totalErrors.toLocaleString()}
              </div>
              <p className="text-sm text-muted-foreground">
                Total errors in period
              </p>
            </div>
            <ChartContainer config={chartConfig} className="h-[400px] w-full">
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
                <ChartTooltip
                  cursor={false}
                  content={<ChartTooltipContent />}
                />
                <Line
                  dataKey="count"
                  type="monotone"
                  stroke="var(--color-count)"
                  strokeWidth={2}
                  dot={{
                    fill: 'var(--color-count)',
                    r: 4,
                  }}
                  activeDot={{
                    r: 6,
                  }}
                />
              </LineChart>
            </ChartContainer>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
