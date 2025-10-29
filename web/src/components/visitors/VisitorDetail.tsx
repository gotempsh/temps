import {
  getVisitorDetailsOptions,
  getVisitorSessions2Options,
  getVisitorSessionsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
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
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  Activity,
  ArrowLeft,
  Bug,
  Calendar,
  ChevronLeft,
  ChevronRight,
  Clock,
  Globe as MapPinIcon,
  MousePointer,
  PlayCircle,
  TrendingUp,
  Users as UserIcon,
} from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'

interface VisitorDetailProps {
  project: ProjectResponse
  visitorId: number
}

export function VisitorDetail({ project, visitorId }: VisitorDetailProps) {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [page, setPage] = React.useState(1)
  const [limit, setLimit] = React.useState(25)

  // Get the active tab from URL or default to 'sessions'
  const activeTab = searchParams.get('tab') || 'sessions'

  // Update URL when tab changes
  const handleTabChange = (value: string) => {
    setSearchParams({ tab: value })
  }

  const {
    data: visitorDetails,
    isLoading: isLoadingDetails,
    error: detailsError,
  } = useQuery({
    ...getVisitorDetailsOptions({
      path: {
        visitor_id: visitorId,
      },
      query: {
        project_id: project.id,
      },
    }),
  })

  const {
    data: sessions,
    isLoading: isLoadingSessions,
    error: sessionsError,
  } = useQuery({
    ...getVisitorSessionsOptions({
      path: {
        visitor_id: visitorId, // UUID string
      },
      query: {
        project_id: project.id,
      },
    }),
    enabled: !!visitorDetails?.visitor_id,
  })

  // Query for session replays
  const {
    data: sessionReplays,
    isLoading: isLoadingReplays,
    error: replaysError,
  } = useQuery({
    ...getVisitorSessions2Options({
      path: {
        visitor_id: visitorDetails?.id as number,
      },
      query: {
        page,
        per_page: limit,
      },
    }),
    enabled: !!visitorDetails?.id,
  })

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${Math.round(seconds)}s`
    const minutes = Math.floor(seconds / 60)
    if (minutes < 60) return `${minutes}m`
    const hours = Math.floor(minutes / 60)
    return `${hours}h ${minutes % 60}m`
  }

  const totalPages = React.useMemo(() => {
    if (!sessions) return 0
    return Math.ceil(sessions.total_sessions / limit)
  }, [sessions, limit])

  const handleLimitChange = (value: string) => {
    setLimit(parseInt(value))
    setPage(1) // Reset to first page when limit changes
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-4">
        <Button
          variant="ghost"
          size="icon"
          onClick={() =>
            navigate(`/projects/${project.slug}/analytics/visitors`)
          }
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div className="flex-1">
          <h2 className="text-2xl font-semibold flex items-center gap-2">
            {visitorDetails?.is_crawler ? (
              <Bug className="h-6 w-6" />
            ) : (
              <UserIcon className="h-6 w-6" />
            )}
            Visitor Details
          </h2>
          <p className="text-muted-foreground">ID: {visitorId}</p>
        </div>
      </div>

      {/* Visitor Info Section - Independent error handling */}
      {isLoadingDetails ? (
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          {[...Array(6)].map((_, i) => (
            <Card key={i}>
              <CardHeader className="pb-2">
                <Skeleton className="h-4 w-24" />
              </CardHeader>
              <CardContent>
                <Skeleton className="h-6 w-32" />
              </CardContent>
            </Card>
          ))}
        </div>
      ) : detailsError ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <p className="text-muted-foreground mb-2">
              Failed to load visitor details
            </p>
            <Button variant="outline" onClick={() => window.location.reload()}>
              Try again
            </Button>
          </CardContent>
        </Card>
      ) : visitorDetails ? (
        <>
          {/* Visitor Info Cards */}
          <div className="grid grid-cols-1 md:grid-cols-3 lg:grid-cols-6 gap-4">
            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <MapPinIcon className="h-3 w-3" />
                  Location
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold">
                  {visitorDetails?.country || 'Unknown'}
                </div>
                {visitorDetails?.city && (
                  <div className="text-sm text-muted-foreground">
                    {visitorDetails.city}
                  </div>
                )}
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <Calendar className="h-3 w-3" />
                  First Seen
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-sm">
                  {visitorDetails &&
                    format(new Date(visitorDetails.first_seen), 'MMM d, yyyy')}
                </div>
                <div className="text-xs text-muted-foreground">
                  {visitorDetails &&
                    format(new Date(visitorDetails.first_seen), 'HH:mm:ss')}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <Clock className="h-3 w-3" />
                  Last Activity
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-sm">
                  {visitorDetails &&
                    format(new Date(visitorDetails.last_seen), 'MMM d, yyyy')}
                </div>
                <div className="text-xs text-muted-foreground">
                  {visitorDetails &&
                    format(new Date(visitorDetails.last_seen), 'HH:mm:ss')}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <Activity className="h-3 w-3" />
                  Sessions
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-2xl">
                  {visitorDetails?.total_sessions}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <MousePointer className="h-3 w-3" />
                  Page Views
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-2xl">
                  {visitorDetails?.total_page_views}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <TrendingUp className="h-3 w-3" />
                  Engagement
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-2xl">
                  {visitorDetails?.engagement_rate.toFixed(1)}%
                </div>
              </CardContent>
            </Card>
          </div>

          {/* Additional Metrics Row */}
          <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
            <Card>
              <CardHeader className="pb-2">
                <CardDescription>Total Events</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-xl">
                  {visitorDetails?.total_events}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription>Bounce Rate</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold text-xl">
                  {visitorDetails?.bounce_rate.toFixed(1)}%
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription>Type</CardDescription>
              </CardHeader>
              <CardContent>
                <Badge
                  variant={visitorDetails?.is_crawler ? 'secondary' : 'default'}
                >
                  {visitorDetails?.is_crawler
                    ? visitorDetails.crawler_name || 'Crawler'
                    : 'Human Visitor'}
                </Badge>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription>User Agent</CardDescription>
              </CardHeader>
              <CardContent>
                <div
                  className="text-sm truncate"
                  title={visitorDetails?.user_agent || 'Unknown'}
                >
                  {visitorDetails?.user_agent || 'Unknown'}
                </div>
              </CardContent>
            </Card>
          </div>

          {/* Custom Data Section */}
          {visitorDetails?.custom_data &&
            Object.keys(visitorDetails.custom_data).length > 0 && (
              <Card>
                <CardHeader>
                  <CardTitle>Custom Data</CardTitle>
                  <CardDescription>
                    Additional visitor information
                  </CardDescription>
                </CardHeader>
                <CardContent>
                  <div className="space-y-3">
                    {Object.entries(
                      visitorDetails.custom_data as Record<string, any>
                    ).map(([key, value]) => (
                      <div
                        key={key}
                        className="flex items-start gap-4 py-2 border-b last:border-0"
                      >
                        <div className="min-w-[140px] text-sm font-medium text-muted-foreground">
                          {key
                            .replace(/_/g, ' ')
                            .replace(/\b\w/g, (l) => l.toUpperCase())}
                        </div>
                        <div className="flex-1 text-sm">
                          {typeof value === 'object' && value !== null ? (
                            <pre className="bg-muted rounded p-2 overflow-x-auto">
                              {JSON.stringify(value, null, 2)}
                            </pre>
                          ) : (
                            <span className="break-words">{String(value)}</span>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                </CardContent>
              </Card>
            )}
        </>
      ) : null}

      {/* Sessions and Replays Tabs */}
      <Tabs
        value={activeTab}
        onValueChange={handleTabChange}
        className="space-y-4"
      >
        <TabsList className="grid w-full max-w-md grid-cols-2">
          <TabsTrigger value="sessions" className="flex items-center gap-2">
            <Activity className="h-4 w-4" />
            Sessions
            {sessions && (
              <Badge variant="secondary" className="ml-1">
                {sessions.total_sessions}
              </Badge>
            )}
          </TabsTrigger>
          <TabsTrigger value="replays" className="flex items-center gap-2">
            <PlayCircle className="h-4 w-4" />
            Session Replays
            {sessionReplays && (
              <Badge variant="secondary" className="ml-1">
                {sessionReplays.sessions?.length || 0}
              </Badge>
            )}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="sessions" className="space-y-4">
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>Sessions</CardTitle>
                  <CardDescription>
                    {sessions
                      ? `${sessions.total_sessions} total sessions`
                      : 'Session history for this visitor'}
                  </CardDescription>
                </div>
                <Select
                  value={limit.toString()}
                  onValueChange={handleLimitChange}
                >
                  <SelectTrigger className="w-[120px]">
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
            </CardHeader>
            <CardContent>
              {isLoadingSessions ? (
                <div className="space-y-2">
                  {[...Array(5)].map((_, i) => (
                    <div key={i} className="flex items-center space-x-4 py-4">
                      <Skeleton className="h-4 w-32" />
                      <Skeleton className="h-4 w-24" />
                      <Skeleton className="h-4 w-20" />
                      <Skeleton className="h-4 w-16" />
                    </div>
                  ))}
                </div>
              ) : sessionsError ? (
                <div className="flex flex-col items-center justify-center py-12">
                  <p className="text-muted-foreground mb-2">
                    Failed to load sessions
                  </p>
                  <p className="text-sm text-muted-foreground mb-4">
                    This might be due to visitor ID format issues
                  </p>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => window.location.reload()}
                  >
                    Try again
                  </Button>
                </div>
              ) : !sessions || sessions.sessions.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-12">
                  <p className="text-muted-foreground">
                    No sessions found for this visitor
                  </p>
                </div>
              ) : (
                <>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Session ID</TableHead>
                        <TableHead>Started</TableHead>
                        <TableHead>Duration</TableHead>
                        <TableHead>Pages</TableHead>
                        <TableHead>Events</TableHead>
                        <TableHead>Entry Page</TableHead>
                        <TableHead>Exit Page</TableHead>
                        <TableHead>Bounce</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {sessions.sessions.map((session) => (
                        <TableRow
                          key={session.session_id}
                          className="cursor-pointer hover:bg-muted/50"
                          onClick={() =>
                            navigate(
                              `/projects/${project.slug}/analytics/visitors/${visitorId}/sessions/${session.session_id.toString()}`
                            )
                          }
                        >
                          <TableCell className="font-mono text-sm">
                            {session.session_id}
                          </TableCell>
                          <TableCell className="text-sm">
                            {session.started_at &&
                            !isNaN(Date.parse(session.started_at))
                              ? format(
                                  new Date(session.started_at),
                                  'MMM d, HH:mm:ss'
                                )
                              : '-'}
                          </TableCell>
                          <TableCell className="text-sm">
                            {formatDuration(session.duration_seconds || 0)}
                          </TableCell>
                          <TableCell className="text-sm">
                            {session.page_views || 0}
                          </TableCell>
                          <TableCell className="text-sm">
                            {session.events_count}
                          </TableCell>
                          <TableCell className="text-sm max-w-[200px] truncate">
                            {session.entry_path || '-'}
                          </TableCell>
                          <TableCell className="text-sm max-w-[200px] truncate">
                            {session.exit_path || '-'}
                          </TableCell>
                          <TableCell>
                            {session.is_bounced ? (
                              <Badge variant="secondary">Yes</Badge>
                            ) : (
                              <Badge variant="outline">No</Badge>
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
                        {Math.min(page * limit, sessions.total_sessions)} of{' '}
                        {sessions.total_sessions} sessions
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
                          {(() => {
                            const pageNumbers = []
                            const maxButtons = 5
                            let startPage = Math.max(
                              1,
                              page - Math.floor(maxButtons / 2)
                            )
                            const endPage = Math.min(
                              totalPages,
                              startPage + maxButtons - 1
                            )

                            if (endPage - startPage < maxButtons - 1) {
                              startPage = Math.max(1, endPage - maxButtons + 1)
                            }

                            for (let i = startPage; i <= endPage; i++) {
                              pageNumbers.push(i)
                            }

                            return pageNumbers.map((pageNum) => (
                              <Button
                                key={pageNum}
                                variant={
                                  pageNum === page ? 'default' : 'outline'
                                }
                                size="sm"
                                onClick={() => setPage(pageNum)}
                                className="w-10"
                              >
                                {pageNum}
                              </Button>
                            ))
                          })()}
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
        </TabsContent>

        <TabsContent value="replays" className="space-y-4">
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>Session Replays</CardTitle>
                  <CardDescription>
                    {sessionReplays
                      ? `${sessionReplays.sessions?.length || 0} replays available`
                      : 'Watch session recordings for this visitor'}
                  </CardDescription>
                </div>
                <Select
                  value={limit.toString()}
                  onValueChange={handleLimitChange}
                >
                  <SelectTrigger className="w-[120px]">
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
            </CardHeader>
            <CardContent>
              {isLoadingReplays ? (
                <div className="space-y-2">
                  {[...Array(5)].map((_, i) => (
                    <div key={i} className="flex items-center space-x-4 py-4">
                      <Skeleton className="h-4 w-32" />
                      <Skeleton className="h-4 w-24" />
                      <Skeleton className="h-4 w-20" />
                      <Skeleton className="h-4 w-16" />
                    </div>
                  ))}
                </div>
              ) : replaysError ? (
                <div className="flex flex-col items-center justify-center py-12">
                  <p className="text-muted-foreground mb-2">
                    Failed to load session replays
                  </p>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => window.location.reload()}
                  >
                    Try again
                  </Button>
                </div>
              ) : !sessionReplays ||
                !sessionReplays.sessions ||
                sessionReplays.sessions.length === 0 ? (
                <div className="flex flex-col items-center justify-center py-12">
                  <PlayCircle className="h-12 w-12 text-muted-foreground mb-4" />
                  <p className="text-muted-foreground mb-2">
                    No session replays available
                  </p>
                  <p className="text-sm text-muted-foreground">
                    Session recordings will appear here when available
                  </p>
                </div>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Session ID</TableHead>
                      <TableHead>Started</TableHead>
                      <TableHead>Duration</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {sessionReplays.sessions.map((session) => (
                      <TableRow
                        key={session.id}
                        className="cursor-pointer hover:bg-muted/50"
                        onClick={() => navigate(`session-replay/${session.id}`)}
                      >
                        <TableCell className="font-mono text-sm">
                          {session.id}
                        </TableCell>
                        <TableCell className="text-sm">
                          {session.created_at &&
                          !isNaN(Date.parse(session.created_at))
                            ? format(
                                new Date(session.created_at),
                                'MMM d, HH:mm:ss'
                              )
                            : '-'}
                        </TableCell>
                        <TableCell className="text-sm">
                          {formatDuration(session.duration || 0)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  )
}
