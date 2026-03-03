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
import {
  Globe,
  Link,
  Mail,
  Megaphone,
  Search,
  Share2,
  Tag,
  Zap,
} from 'lucide-react'
import * as React from 'react'

const CHANNEL_ICONS: Record<string, LucideIcon> = {
  Direct: Link,
  Organic: Search,
  'Organic Search': Search,
  Paid: Megaphone,
  'Paid Search': Megaphone,
  Social: Share2,
  Referral: Globe,
  Email: Mail,
  Display: Tag,
  Affiliate: Zap,
}

const CHANNEL_COLORS: Record<string, string> = {
  Direct: 'hsl(var(--chart-1))',
  Organic: 'hsl(var(--chart-2))',
  'Organic Search': 'hsl(var(--chart-2))',
  Paid: 'hsl(var(--chart-3))',
  'Paid Search': 'hsl(var(--chart-3))',
  Social: 'hsl(var(--chart-4))',
  Referral: 'hsl(var(--chart-5))',
  Email: 'hsl(var(--chart-1))',
}

interface ChannelsChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function ChannelsChart({
  project,
  startDate,
  endDate,
  environment,
}: ChannelsChartProps) {
  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'channel',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedChannels = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        channel: item.value || 'Unknown',
        count: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <CardTitle>Traffic Channels</CardTitle>
        <CardDescription>
          {startDate && endDate
            ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
            : 'Select a date range'}
        </CardDescription>
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
              Failed to load channel analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedChannels.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3" style={{ minHeight: '400px' }}>
            {sortedChannels.map((channel) => {
              const Icon = CHANNEL_ICONS[channel.channel] || Globe
              const color = CHANNEL_COLORS[channel.channel]
              return (
                <div key={channel.channel} className="space-y-2">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      <Icon className="h-5 w-5 text-muted-foreground" />
                      <span className="text-sm font-medium">
                        {channel.channel}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <span className="text-sm text-muted-foreground">
                        {channel.percentage}%
                      </span>
                      <span className="text-sm font-mono text-muted-foreground">
                        {channel.count.toLocaleString()}
                      </span>
                    </div>
                  </div>
                  <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                    <div
                      className="absolute inset-y-0 left-0 rounded-full transition-all duration-500 bg-primary"
                      style={
                        color
                          ? {
                              width: `${channel.percentage}%`,
                              backgroundColor: color,
                            }
                          : { width: `${channel.percentage}%` }
                      }
                    />
                  </div>
                </div>
              )
            })}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedChannels.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedChannels.length} channels by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
