import { getLiveVisitorsListOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { Globe, Users as UserIcon, RefreshCw, ChevronRight } from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import { Skeleton } from '@/components/ui/skeleton'
import { useState } from 'react'

interface LiveVisitorsListProps {
  project: ProjectResponse
}

export function LiveVisitorsList({ project }: LiveVisitorsListProps) {
  const navigate = useNavigate()
  const [autoRefresh, setAutoRefresh] = useState(true)

  const { data, isLoading, error, refetch } = useQuery({
    ...getLiveVisitorsListOptions({
      query: {
        project_id: project.id,
      },
    }),
    refetchInterval: autoRefresh ? 2000 : false,
  })

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle>Live Visitors</CardTitle>
              <CardDescription>
                {data
                  ? `${data.visitors.length} visitor${data.visitors.length !== 1 ? 's' : ''} online now`
                  : 'Real-time active visitors on your site'}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant={autoRefresh ? 'default' : 'outline'}
                size="sm"
                onClick={() => setAutoRefresh(!autoRefresh)}
                title={
                  autoRefresh ? 'Auto-refresh enabled' : 'Auto-refresh disabled'
                }
              >
                <RefreshCw
                  className={`h-4 w-4 ${autoRefresh ? 'animate-spin' : ''}`}
                />
                {autoRefresh ? 'Live' : 'Paused'}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => refetch()}
                disabled={isLoading}
              >
                Refresh
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
              {[...Array(3)].map((_, i) => (
                <Skeleton key={i} className="h-40 w-full rounded-lg" />
              ))}
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-12">
              <p className="text-muted-foreground mb-2">
                Failed to load live visitors
              </p>
              <Button variant="outline" onClick={() => refetch()}>
                Try again
              </Button>
            </div>
          ) : !data || data.visitors.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12">
              <UserIcon className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-muted-foreground">
                No visitors currently active
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              {data.visitors.map((visitor) => (
                <Card
                  key={visitor.visitor_id}
                  className="cursor-pointer hover:shadow-md hover:border-primary/60 transition-all border-l-4 border-l-green-500"
                  onClick={() =>
                    navigate(
                      `/projects/${project.slug}/analytics/visitors/${visitor.id}`
                    )
                  }
                >
                  <CardContent className="p-5 space-y-4">
                    {/* Header with status indicator and visitor type */}
                    <div className="flex items-start justify-between gap-3">
                      <div className="flex items-center gap-3 flex-1 min-w-0">
                        {/* Avatar/Icon */}
                        <div
                          className={`p-2.5 rounded-lg flex-shrink-0 ${
                            visitor.is_crawler
                              ? 'bg-amber-100 dark:bg-amber-900/30'
                              : 'bg-blue-100 dark:bg-blue-900/30'
                          }`}
                        >
                          <UserIcon
                            className={`h-5 w-5 ${
                              visitor.is_crawler
                                ? 'text-amber-600 dark:text-amber-400'
                                : 'text-blue-600 dark:text-blue-400'
                            }`}
                          />
                        </div>

                        {/* Visitor ID and Type */}
                        <div className="min-w-0 flex-1">
                          <div className="font-mono text-xs text-muted-foreground truncate">
                            {visitor.visitor_id?.substring(0, 12)}
                          </div>
                          <p className="text-sm font-medium text-foreground mt-1">
                            {visitor.is_crawler ? 'Bot' : 'Human Visitor'}
                            {visitor.crawler_name &&
                              ` - ${visitor.crawler_name}`}
                          </p>
                        </div>
                      </div>

                      {/* Live indicator */}
                      <div className="flex-shrink-0 flex items-center gap-1.5">
                        <div
                          className="h-2.5 w-2.5 rounded-full bg-green-500 animate-pulse"
                          title="Active now"
                        />
                        <span className="text-xs font-medium text-green-600 dark:text-green-400">
                          Live
                        </span>
                      </div>
                    </div>

                    {/* Browser and IP info */}
                    <div className="grid grid-cols-2 gap-3 pt-2 border-t">
                      <div>
                        <p className="text-xs text-muted-foreground font-medium mb-1">
                          Browser
                        </p>
                        <p className="text-sm font-medium">
                          {visitor.user_agent
                            ? getBrowserName(visitor.user_agent)
                            : 'Unknown'}
                        </p>
                      </div>
                      <div>
                        <p className="text-xs text-muted-foreground font-medium mb-1">
                          IP Address
                        </p>
                        <p className="text-sm font-medium font-mono">
                          {visitor.ip_address}
                        </p>
                      </div>
                    </div>

                    {/* Location info */}
                    <div className="pt-2 border-t">
                      <p className="text-xs text-muted-foreground font-medium mb-2">
                        Location
                      </p>
                      <div className="flex items-start gap-2">
                        <Globe className="h-4 w-4 text-muted-foreground flex-shrink-0 mt-0.5" />
                        <div className="text-sm font-medium">
                          <div>
                            {visitor.city}, {visitor.region}
                          </div>
                          <div className="text-xs text-muted-foreground">
                            {visitor.country}
                            {visitor.is_eu && ' (EU)'}
                          </div>
                        </div>
                      </div>
                    </div>

                    {/* Timezone */}
                    <div className="pt-2 border-t grid grid-cols-2 gap-3">
                      <div>
                        <p className="text-xs text-muted-foreground font-medium mb-1">
                          Timezone
                        </p>
                        <p className="text-sm font-medium">
                          {visitor.timezone}
                        </p>
                      </div>
                      <div>
                        <p className="text-xs text-muted-foreground font-medium mb-1">
                          Last Seen
                        </p>
                        <p className="text-sm font-medium">
                          {formatRelativeTime(visitor.last_seen)}
                        </p>
                      </div>
                    </div>

                    {/* View details footer */}
                    <div className="pt-2 border-t flex items-center justify-between">
                      <span className="text-xs text-muted-foreground">
                        View full details
                      </span>
                      <ChevronRight className="h-4 w-4 text-muted-foreground" />
                    </div>
                  </CardContent>
                </Card>
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

// Helper function to format relative time
function formatRelativeTime(isoString: string): string {
  try {
    const date = new Date(isoString)
    const now = new Date()
    const seconds = Math.floor((now.getTime() - date.getTime()) / 1000)

    if (seconds < 60) return 'just now'
    const minutes = Math.floor(seconds / 60)
    if (minutes < 60) return `${minutes}m ago`
    const hours = Math.floor(minutes / 60)
    if (hours < 24) return `${hours}h ago`
    const days = Math.floor(hours / 24)
    return `${days}d ago`
  } catch {
    return 'unknown'
  }
}

// Helper function to extract browser name from user agent
function getBrowserName(userAgent: string): string {
  if (userAgent.includes('Chrome') && !userAgent.includes('Chromium')) {
    return 'Chrome'
  } else if (userAgent.includes('Safari') && !userAgent.includes('Chrome')) {
    return 'Safari'
  } else if (userAgent.includes('Firefox')) {
    return 'Firefox'
  } else if (userAgent.includes('Edge')) {
    return 'Edge'
  } else if (userAgent.includes('Opera') || userAgent.includes('OPR')) {
    return 'Opera'
  } else if (userAgent.includes('bot') || userAgent.includes('Bot')) {
    return 'Bot'
  }
  return 'Unknown'
}
