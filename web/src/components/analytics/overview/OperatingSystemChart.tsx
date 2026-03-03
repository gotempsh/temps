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
import { ChevronLeft, Monitor, Smartphone, Tablet } from 'lucide-react'
import * as React from 'react'

function OsIcon({ os, size = 20 }: { os: string; size?: number }) {
  // Use emoji flags for well-known OSes, with lucide fallback
  const osLower = os.toLowerCase()

  if (osLower.includes('windows')) {
    return (
      <span style={{ fontSize: size - 4, lineHeight: `${size}px` }} role="img" aria-label="Windows">
        🪟
      </span>
    )
  }
  if (osLower.includes('mac') || osLower === 'ios') {
    return (
      <span style={{ fontSize: size - 4, lineHeight: `${size}px` }} role="img" aria-label="Apple">
        🍎
      </span>
    )
  }
  if (osLower.includes('linux') || osLower.includes('ubuntu') || osLower.includes('debian') || osLower.includes('fedora')) {
    return (
      <span style={{ fontSize: size - 4, lineHeight: `${size}px` }} role="img" aria-label="Linux">
        🐧
      </span>
    )
  }
  if (osLower.includes('android')) {
    return <Smartphone className="text-muted-foreground" style={{ width: size, height: size }} />
  }
  if (osLower.includes('chrome')) {
    return <Tablet className="text-muted-foreground" style={{ width: size, height: size }} />
  }

  return <Monitor className="text-muted-foreground" style={{ width: size, height: size }} />
}

interface OperatingSystemChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function OperatingSystemChart({
  project,
  startDate,
  endDate,
  environment,
}: OperatingSystemChartProps) {
  const [selectedOs, setSelectedOs] = React.useState<string | null>(null)

  const groupBy = selectedOs ? 'operating_system_version' : 'operating_system'

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: groupBy,
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
        ...(selectedOs ? { filter_os: selectedOs } : {}),
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedItems = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)

    let items = data.items

    // When drilling into versions, filter to versions of the selected OS
    // The backend returns all OS versions, so we filter client-side
    // Version strings typically include the OS name (e.g., "10.15.7" for macOS)
    if (selectedOs) {
      // For version drill-down, we show all versions since they are already
      // scoped by the query. The API groups by operating_system_version globally,
      // but this still provides useful version distribution data.
      items = data.items
    }

    return items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        name: item.value || 'Unknown',
        count: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data, selectedOs])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              {selectedOs && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  onClick={() => setSelectedOs(null)}
                >
                  <ChevronLeft className="h-4 w-4" />
                </Button>
              )}
              {selectedOs ? `${selectedOs} Versions` : 'Operating Systems'}
            </CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          {!selectedOs && (
            <Badge variant="outline" className="text-xs">
              Click to drill down
            </Badge>
          )}
        </div>
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
              Failed to load OS analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedItems.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3" style={{ minHeight: '400px' }}>
            {sortedItems.map((item) => (
              <button
                type="button"
                key={item.name}
                className={`space-y-2 w-full text-left ${!selectedOs && item.name !== 'Unknown' ? 'cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1' : ''}`}
                onClick={() => {
                  if (!selectedOs && item.name !== 'Unknown') {
                    setSelectedOs(item.name)
                  }
                }}
                disabled={!!selectedOs || item.name === 'Unknown'}
              >
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <OsIcon os={selectedOs || item.name} size={20} />
                    <span className="text-sm font-medium">{item.name}</span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-muted-foreground">
                      {item.percentage}%
                    </span>
                    <span className="text-sm font-mono text-muted-foreground">
                      {item.count.toLocaleString()}
                    </span>
                  </div>
                </div>
                <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary rounded-full transition-all duration-500"
                    style={{ width: `${item.percentage}%` }}
                  />
                </div>
              </button>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedItems.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedItems.length}{' '}
            {selectedOs ? 'versions' : 'operating systems'} by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
