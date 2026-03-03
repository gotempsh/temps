import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import type { LucideIcon } from 'lucide-react'
import { Monitor, Smartphone, Tablet } from 'lucide-react'
import * as React from 'react'

const DEVICE_ICONS: Record<string, LucideIcon> = {
  Desktop: Monitor,
  Mobile: Smartphone,
  Tablet: Tablet,
}

const DEVICE_COLORS: Record<string, string> = {
  Desktop: 'hsl(var(--chart-1))',
  Mobile: 'hsl(var(--chart-2))',
  Tablet: 'hsl(var(--chart-3))',
}

interface DevicesChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function DevicesChart({
  project,
  startDate,
  endDate,
  environment,
}: DevicesChartProps) {
  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'device_type',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedDevices = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        device: item.value || 'Unknown',
        count: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
        percentageNum: (item.count / total) * 100,
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <CardTitle>Devices</CardTitle>
        <CardDescription>
          {startDate && endDate
            ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
            : 'Select a date range'}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            {[...Array(3)].map((_, i) => (
              <div key={`skeleton-${i}`} className="flex items-center justify-between">
                <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load device analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedDevices.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-6">
            {/* Visual bars */}
            <div className="space-y-3">
              {sortedDevices.map((device) => {
                const Icon = DEVICE_ICONS[device.device] || Monitor
                const color = DEVICE_COLORS[device.device]
                return (
                  <div key={device.device} className="space-y-2">
                    <div className="flex items-center justify-between">
                      <div className="flex items-center gap-3">
                        <Icon className="h-5 w-5 text-muted-foreground" />
                        <span className="text-sm font-medium">
                          {device.device}
                        </span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-sm text-muted-foreground">
                          {device.percentage}%
                        </span>
                        <span className="text-sm font-mono text-muted-foreground">
                          {device.count.toLocaleString()}
                        </span>
                      </div>
                    </div>
                    <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                      <div
                        className="absolute inset-y-0 left-0 rounded-full transition-all duration-500 bg-primary"
                        style={
                          color
                            ? {
                                width: `${device.percentage}%`,
                                backgroundColor: color,
                              }
                            : { width: `${device.percentage}%` }
                        }
                      />
                    </div>
                  </div>
                )
              })}
            </div>
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedDevices.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Device type breakdown by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
