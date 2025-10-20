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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Badge } from '@/components/ui/badge'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  Globe,
  Clock,
  MousePointer,
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

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${Math.round(seconds)}s`
    const minutes = Math.floor(seconds / 60)
    if (minutes < 60) return `${minutes}m`
    const hours = Math.floor(minutes / 60)
    return `${hours}h ${minutes % 60}m`
  }

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
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div key={i} className="flex items-center space-x-4 py-4">
                  <Skeleton className="h-10 w-10 rounded-full" />
                  <div className="flex-1 space-y-2">
                    <Skeleton className="h-4 w-[200px]" />
                    <Skeleton className="h-3 w-[150px]" />
                  </div>
                  <Skeleton className="h-4 w-[100px]" />
                </div>
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
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Visitor</TableHead>
                    <TableHead>Location</TableHead>
                    <TableHead>User Agent</TableHead>
                    <TableHead>Sessions</TableHead>
                    <TableHead>Page Views</TableHead>
                    <TableHead>Total Time</TableHead>
                    <TableHead>First Seen</TableHead>
                    <TableHead>Last Seen</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {data.visitors.map((visitor: VisitorInfo) => (
                    <TableRow
                      key={visitor.visitor_id}
                      className="cursor-pointer hover:bg-muted/50"
                      onClick={() => {
                        navigate(
                          `/projects/${project.slug}/analytics/visitors/${visitor.id}`
                        )
                      }}
                    >
                      <TableCell>
                        <div className="flex items-center gap-2">
                          {visitor.is_crawler ? (
                            <Bug className="h-4 w-4 text-muted-foreground" />
                          ) : (
                            <UserIcon className="h-4 w-4 text-muted-foreground" />
                          )}
                          <div>
                            <div className="font-medium text-sm">
                              {visitor.visitor_id.substring(0, 8)}...
                            </div>
                            {visitor.crawler_name && (
                              <Badge variant="outline" className="text-xs">
                                {visitor.crawler_name}
                              </Badge>
                            )}
                          </div>
                        </div>
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-1">
                          <Globe className="h-3 w-3 text-muted-foreground" />
                          <span className="text-sm">
                            {visitor.location || 'Unknown'}
                          </span>
                        </div>
                      </TableCell>
                      <TableCell>
                        <div
                          className="text-sm truncate max-w-[200px]"
                          title={visitor.user_agent || 'Unknown'}
                        >
                          {visitor.user_agent
                            ? visitor.user_agent.includes('Chrome')
                              ? 'Chrome'
                              : visitor.user_agent.includes('Safari') &&
                                  !visitor.user_agent.includes('Chrome')
                                ? 'Safari'
                                : visitor.user_agent.includes('Firefox')
                                  ? 'Firefox'
                                  : visitor.user_agent.includes('Edge')
                                    ? 'Edge'
                                    : visitor.user_agent.includes('bot') ||
                                        visitor.user_agent.includes('Bot')
                                      ? 'Bot'
                                      : 'Other'
                            : 'Unknown'}
                        </div>
                      </TableCell>
                      <TableCell>
                        <div className="text-sm font-medium">
                          {visitor.sessions_count}
                        </div>
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-1">
                          <MousePointer className="h-3 w-3 text-muted-foreground" />
                          <span className="text-sm">{visitor.page_views}</span>
                        </div>
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-1">
                          <Clock className="h-3 w-3 text-muted-foreground" />
                          <span className="text-sm">
                            {formatDuration(visitor.total_time_seconds)}
                          </span>
                        </div>
                      </TableCell>
                      <TableCell className="text-sm text-muted-foreground">
                        {format(
                          new Date(visitor.first_seen),
                          'MMM d, yyyy HH:mm'
                        )}
                      </TableCell>
                      <TableCell className="text-sm text-muted-foreground">
                        {format(
                          new Date(visitor.last_seen),
                          'MMM d, yyyy HH:mm'
                        )}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>

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
