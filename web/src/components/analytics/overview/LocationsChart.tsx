import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
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
import * as React from 'react'

type LocationType = 'country' | 'region' | 'city'

interface LocationsChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function LocationsChart({
  project,
  startDate,
  endDate,
  environment,
}: LocationsChartProps) {
  const [locationType, setLocationType] =
    React.useState<LocationType>('country')

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: locationType,
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .slice(0, 5)
      .map((item) => ({
        location: item.value || 'Unknown',
        visitors: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
        // color: `var(--chart-${item.value})`,
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Locations</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <div className="flex gap-1">
            <Badge
              variant={locationType === 'country' ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setLocationType('country')}
            >
              Country
            </Badge>
            <Badge
              variant={locationType === 'region' ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setLocationType('region')}
            >
              Region
            </Badge>
            <Badge
              variant={locationType === 'city' ? 'default' : 'outline'}
              className="cursor-pointer text-xs"
              onClick={() => setLocationType('city')}
            >
              City
            </Badge>
          </div>
        </div>
      </CardHeader>
      <CardContent className="space-y-6">
        {isLoading ? (
          <div className="space-y-4">
            {[...Array(5)].map((_, i) => (
              <div key={i} className="space-y-2">
                <div className="flex items-center justify-between">
                  <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
                  <div className="h-4 w-[60px] bg-muted animate-pulse rounded" />
                </div>
                <div className="h-2 w-full bg-muted animate-pulse rounded-full" />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load location analytics
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
          <div className="space-y-4">
            {chartData.map((location) => (
              <div key={location.location} className="flex items-center">
                <div className="w-full">
                  <div className="flex items-center justify-between mb-1">
                    <span className="text-sm font-medium">
                      {location.location}
                    </span>
                    <span className="text-sm text-muted-foreground">
                      {location.visitors.toLocaleString()} (
                      {location.percentage}%)
                    </span>
                  </div>
                  <div className="w-full h-2 bg-muted rounded-full overflow-hidden">
                    <div
                      className="h-full transition-all"
                      style={{
                        width: `${location.percentage}%`,
                        backgroundColor: location.color,
                      }}
                    />
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && chartData.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {chartData.length} locations by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
