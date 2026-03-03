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
import { ChevronLeft, ChevronRight, MapPin } from 'lucide-react'
import * as React from 'react'

type LocationType = 'country' | 'region' | 'city'

interface DrillState {
  level: LocationType
  country?: string
  region?: string
}

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
  const [drill, setDrill] = React.useState<DrillState>({ level: 'country' })

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: drill.level,
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
        ...(drill.country ? { filter_country: drill.country } : {}),
        ...(drill.region ? { filter_region: drill.region } : {}),
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        location: item.value || 'Unknown',
        visitors: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data])

  function handleClick(locationName: string) {
    if (drill.level === 'country' && locationName !== 'Unknown') {
      setDrill({ level: 'region', country: locationName })
    } else if (drill.level === 'region' && locationName !== 'Unknown') {
      setDrill({ level: 'city', country: drill.country, region: locationName })
    }
  }

  function handleBack() {
    if (drill.level === 'city') {
      setDrill({ level: 'region', country: drill.country })
    } else if (drill.level === 'region') {
      setDrill({ level: 'country' })
    }
  }

  const canDrillDown = drill.level !== 'city'
  const canGoBack = drill.level !== 'country'

  const breadcrumb = React.useMemo(() => {
    const parts: string[] = []
    if (drill.country) parts.push(drill.country)
    if (drill.region) parts.push(drill.region)
    return parts
  }, [drill])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              {canGoBack && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  onClick={handleBack}
                >
                  <ChevronLeft className="h-4 w-4" />
                </Button>
              )}
              Locations
            </CardTitle>
            <CardDescription>
              {breadcrumb.length > 0 ? (
                <span className="flex items-center gap-1 flex-wrap">
                  <button
                    type="button"
                    className="text-primary hover:underline cursor-pointer"
                    onClick={() => setDrill({ level: 'country' })}
                  >
                    All Countries
                  </button>
                  {drill.country && (
                    <>
                      <ChevronRight className="h-3 w-3" />
                      <button
                        type="button"
                        className={`${drill.level === 'region' ? 'font-medium' : 'text-primary hover:underline cursor-pointer'}`}
                        onClick={() => {
                          if (drill.level !== 'region') {
                            setDrill({
                              level: 'region',
                              country: drill.country,
                            })
                          }
                        }}
                      >
                        {drill.country}
                      </button>
                    </>
                  )}
                  {drill.region && (
                    <>
                      <ChevronRight className="h-3 w-3" />
                      <span className="font-medium">{drill.region}</span>
                    </>
                  )}
                </span>
              ) : startDate && endDate ? (
                `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
              ) : (
                'Select a date range'
              )}
            </CardDescription>
          </div>
          {canDrillDown && !canGoBack && (
            <Badge variant="outline" className="text-xs">
              Click to drill down
            </Badge>
          )}
        </div>
      </CardHeader>
      <CardContent className="space-y-6">
        {isLoading ? (
          <div className="space-y-4">
            {[...Array(5)].map((_, i) => (
              <div key={`skeleton-loc-${i}`} className="space-y-2">
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
              <button
                type="button"
                key={location.location}
                className={`flex items-center w-full text-left ${canDrillDown && location.location !== 'Unknown' ? 'cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1' : ''}`}
                onClick={() => handleClick(location.location)}
                disabled={!canDrillDown || location.location === 'Unknown'}
              >
                <div className="w-full">
                  <div className="flex items-center justify-between mb-1">
                    <div className="flex items-center gap-2">
                      <MapPin className="h-3 w-3 text-muted-foreground" />
                      <span className="text-sm font-medium">
                        {location.location}
                      </span>
                      {canDrillDown && location.location !== 'Unknown' && (
                        <ChevronRight className="h-3 w-3 text-muted-foreground" />
                      )}
                    </div>
                    <span className="text-sm text-muted-foreground">
                      {location.visitors.toLocaleString()} (
                      {location.percentage}%)
                    </span>
                  </div>
                  <div className="w-full h-2 bg-muted rounded-full overflow-hidden">
                    <div
                      className="h-full bg-primary transition-all rounded-full"
                      style={{
                        width: `${location.percentage}%`,
                      }}
                    />
                  </div>
                </div>
              </button>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && chartData.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {chartData.length}{' '}
            {drill.level === 'country'
              ? 'countries'
              : drill.level === 'region'
                ? 'regions'
                : 'cities'}{' '}
            by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
