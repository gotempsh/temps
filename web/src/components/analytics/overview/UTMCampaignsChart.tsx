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
import { Megaphone } from 'lucide-react'
import * as React from 'react'

type UtmDimension =
  | 'utm_source'
  | 'utm_medium'
  | 'utm_campaign'
  | 'utm_term'
  | 'utm_content'

const UTM_LABELS: Record<UtmDimension, string> = {
  utm_source: 'Source',
  utm_medium: 'Medium',
  utm_campaign: 'Campaign',
  utm_term: 'Term',
  utm_content: 'Content',
}

interface UTMCampaignsChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function UTMCampaignsChart({
  project,
  startDate,
  endDate,
  environment,
}: UTMCampaignsChartProps) {
  const [dimension, setDimension] = React.useState<UtmDimension>('utm_source')

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: dimension,
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedItems = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .filter((item) => item.value) // Filter out empty UTM values
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        value: item.value || 'Unknown',
        count: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>UTM Campaigns</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <div className="flex flex-wrap gap-1">
            {(Object.keys(UTM_LABELS) as UtmDimension[]).map((dim) => (
              <Badge
                key={dim}
                variant={dimension === dim ? 'default' : 'outline'}
                className="cursor-pointer text-xs"
                onClick={() => setDimension(dim)}
              >
                {UTM_LABELS[dim]}
              </Badge>
            ))}
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            {[...Array(5)].map((_, i) => (
              <div key={`skeleton-${i}`} className="flex items-center justify-between">
                <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load UTM analytics
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
              No UTM data available for the selected period
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              UTM parameters are tracked when visitors arrive via links with
              utm_source, utm_medium, etc.
            </p>
          </div>
        ) : (
          <div className="space-y-3" style={{ minHeight: '400px' }}>
            {sortedItems.map((item) => (
              <div key={item.value} className="space-y-2">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <Megaphone className="h-4 w-4 text-muted-foreground" />
                    <span className="text-sm font-medium truncate max-w-[250px]">
                      {item.value}
                    </span>
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
              </div>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedItems.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedItems.length} UTM {UTM_LABELS[dimension].toLowerCase()}s by unique
            visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
