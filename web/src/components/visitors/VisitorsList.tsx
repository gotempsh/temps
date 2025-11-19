import { getVisitorsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse, VisitorInfo } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  Globe,
  Bug,
  Users as UserIcon,
  ChevronLeft,
  ChevronRight,
} from 'lucide-react'
import * as React from 'react'
import { useNavigate } from 'react-router-dom'
import { Skeleton } from '@/components/ui/skeleton'

interface VisitorsListProps {
  project: ProjectResponse
}

export function VisitorsList({ project }: VisitorsListProps) {
  const navigate = useNavigate()
  const [page, setPage] = React.useState(1)
  const [limit, setLimit] = React.useState(25)
  const [crawlerFilter, setCrawlerFilter] = React.useState<
    'all' | 'humans' | 'crawlers'
  >('all')

  // Default date range: last 30 days
  const endDate = React.useMemo(() => {
    const date = new Date()
    date.setHours(23, 59, 59, 999)
    return date
  }, [])

  const startDate = React.useMemo(() => {
    const date = new Date()
    date.setDate(date.getDate() - 30)
    date.setHours(0, 0, 0, 0)
    return date
  }, [])

  const { data, isLoading, error, refetch } = useQuery({
    ...getVisitorsOptions({
      query: {
        project_id: project.id,
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        offset: (page - 1) * limit,
        limit,
        include_crawlers:
          crawlerFilter === 'all'
            ? undefined
            : crawlerFilter === 'crawlers'
              ? true
              : false,
      },
    }),
  })

  const totalPages = React.useMemo(() => {
    if (!data) return 0
    return Math.ceil(data.filtered_count / limit)
  }, [data, limit])

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle>Visitors</CardTitle>
              <CardDescription>
                {data
                  ? `${data.filtered_count.toLocaleString()} visitors found`
                  : 'Browse and analyze visitor sessions'}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Select
                value={crawlerFilter}
                onValueChange={(value: any) => setCrawlerFilter(value)}
              >
                <SelectTrigger className="w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Visitors</SelectItem>
                  <SelectItem value="humans">Humans Only</SelectItem>
                  <SelectItem value="crawlers">Crawlers Only</SelectItem>
                </SelectContent>
              </Select>
              <Select
                value={limit.toString()}
                onValueChange={(value) => setLimit(parseInt(value))}
              >
                <SelectTrigger className="w-[100px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="10">10 / page</SelectItem>
                  <SelectItem value="25">25 / page</SelectItem>
                  <SelectItem value="50">50 / page</SelectItem>
                  <SelectItem value="100">100 / page</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              {[...Array(4)].map((_, i) => (
                <Skeleton key={i} className="h-96 w-full rounded-lg" />
              ))}
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-12">
              <p className="text-muted-foreground mb-2">
                Failed to load visitors
              </p>
              <Button variant="outline" onClick={() => refetch()}>
                Try again
              </Button>
            </div>
          ) : !data || data.visitors.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12">
              <UserIcon className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-muted-foreground">No visitors found</p>
            </div>
          ) : (
            <>
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
                {data.visitors.map((visitor: VisitorInfo) => (
                  <Card
                    key={visitor.visitor_id}
                    className="cursor-pointer hover:shadow-md hover:border-primary/60 transition-all border-l-4 border-l-blue-500"
                    onClick={() => {
                      navigate(
                        `/projects/${project.slug}/analytics/visitors/${visitor.id}`
                      )
                    }}
                  >
                    <CardContent className="p-5 space-y-4">
                      {/* Header with visitor type indicator */}
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
                            {visitor.is_crawler ? (
                              <Bug
                                className={`h-5 w-5 text-amber-600 dark:text-amber-400`}
                              />
                            ) : (
                              <UserIcon
                                className={`h-5 w-5 text-blue-600 dark:text-blue-400`}
                              />
                            )}
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
                      </div>
                      {/* Browser and Page Views */}
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
                      </div>

                      {/* Location info */}
                      <div className="pt-2 border-t">
                        <p className="text-xs text-muted-foreground font-medium mb-2">
                          Location
                        </p>
                        <div className="flex items-center gap-2">
                          <Globe className="h-4 w-4 text-muted-foreground flex-shrink-0" />
                          <span className="text-sm font-medium">
                            {visitor.city}, {visitor.region}
                          </span>
                          <span className="text-xs text-muted-foreground">
                            {visitor.country}
                            {visitor.is_eu && ' (EU)'}
                          </span>
                        </div>
                      </div>

                      {/* First and Last Seen */}
                      <div className="pt-2 border-t grid grid-cols-2 gap-3">
                        <div>
                          <p className="text-xs text-muted-foreground font-medium mb-1">
                            First Seen
                          </p>
                          <p className="text-sm font-medium">
                            {format(
                              new Date(visitor.first_seen),
                              'MMM d, HH:mm'
                            )}
                          </p>
                        </div>
                        <div>
                          <p className="text-xs text-muted-foreground font-medium mb-1">
                            Last Seen
                          </p>
                          <p className="text-sm font-medium">
                            {format(
                              new Date(visitor.last_seen),
                              'MMM d, HH:mm'
                            )}
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

              {/* Pagination */}
              {totalPages > 1 && (
                <div className="flex items-center justify-between mt-6">
                  <div className="text-sm text-muted-foreground">
                    Showing {(page - 1) * limit + 1} to{' '}
                    {Math.min(page * limit, data.filtered_count)} of{' '}
                    {data.filtered_count} visitors
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setPage((p) => Math.max(1, p - 1))}
                      disabled={page === 1}
                    >
                      <ChevronLeft className="h-4 w-4" />
                      Previous
                    </Button>
                    <div className="flex items-center gap-1">
                      {[...Array(Math.min(5, totalPages))].map((_, idx) => {
                        const pageNum = page - 2 + idx
                        if (pageNum < 1 || pageNum > totalPages) return null
                        return (
                          <Button
                            key={pageNum}
                            variant={pageNum === page ? 'default' : 'outline'}
                            size="sm"
                            onClick={() => setPage(pageNum)}
                            className="w-10"
                          >
                            {pageNum}
                          </Button>
                        )
                      })}
                    </div>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setPage((p) => Math.min(totalPages, p + 1))
                      }
                      disabled={page === totalPages}
                    >
                      Next
                      <ChevronRight className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              )}
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
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
