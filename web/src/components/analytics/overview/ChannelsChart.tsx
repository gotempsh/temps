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
import type { LucideIcon } from 'lucide-react'
import {
  ChevronLeft,
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

function ReferrerIcon({
  domain,
  className = 'h-5 w-5',
}: {
  domain: string
  className?: string
}) {
  const [hasError, setHasError] = React.useState(false)

  if (!domain || domain === 'Direct' || domain === 'Unknown') {
    return <Globe className={`${className} text-muted-foreground`} />
  }

  if (hasError) {
    return <Globe className={`${className} text-muted-foreground`} />
  }

  const faviconDomain = ['twitter.com', 't.co'].includes(domain)
    ? 'x.com'
    : domain
  const faviconUrl = `https://www.google.com/s2/favicons?domain=${encodeURIComponent(faviconDomain)}&sz=32`

  return (
    <img
      src={faviconUrl}
      alt={`${domain} favicon`}
      className={className}
      onError={() => setHasError(true)}
    />
  )
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
  const [selectedChannel, setSelectedChannel] = React.useState<string | null>(
    null,
  )

  // Main query: channel list or referrer_hostname drill-down
  const groupBy = selectedChannel ? 'referrer_hostname' : 'channel'

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
        ...(selectedChannel ? { filter_channel: selectedChannel } : {}),
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedItems = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
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
            <CardTitle className="flex items-center gap-2">
              {selectedChannel && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  onClick={() => setSelectedChannel(null)}
                >
                  <ChevronLeft className="h-4 w-4" />
                </Button>
              )}
              {selectedChannel
                ? `${selectedChannel} Sources`
                : 'Traffic Channels'}
            </CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          {!selectedChannel && (
            <Badge variant="outline" className="text-xs">
              Click to drill down
            </Badge>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            {[...Array(5)].map((_, i) => (
              <div
                key={`skeleton-${i}`}
                className="flex items-center justify-between"
              >
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
        ) : !sortedItems.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3" style={{ minHeight: '400px' }}>
            {sortedItems.map((item) => {
              const Icon = selectedChannel
                ? null
                : CHANNEL_ICONS[item.value] || Globe
              const color = selectedChannel
                ? undefined
                : CHANNEL_COLORS[item.value]

              return (
                <button
                  type="button"
                  key={item.value}
                  className={`space-y-2 w-full text-left ${
                    !selectedChannel && item.value !== 'Unknown'
                      ? 'cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1'
                      : selectedChannel
                        ? 'p-1 -mx-1'
                        : ''
                  }`}
                  onClick={() => {
                    if (!selectedChannel && item.value !== 'Unknown') {
                      setSelectedChannel(item.value)
                    }
                  }}
                  disabled={!!selectedChannel || item.value === 'Unknown'}
                >
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      {selectedChannel ? (
                        <ReferrerIcon
                          domain={item.value}
                          className="h-5 w-5"
                        />
                      ) : Icon ? (
                        <Icon className="h-5 w-5 text-muted-foreground" />
                      ) : null}
                      <span className="text-sm font-medium">{item.value}</span>
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
                      className="absolute inset-y-0 left-0 rounded-full transition-all duration-500 bg-primary"
                      style={
                        color
                          ? {
                              width: `${item.percentage}%`,
                              backgroundColor: color,
                            }
                          : { width: `${item.percentage}%` }
                      }
                    />
                  </div>
                </button>
              )
            })}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedItems.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedItems.length}{' '}
            {selectedChannel ? 'sources' : 'channels'} by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}
