import {
  getSessionDetailsOptions,
  getVisitorDetailsOptions,
  getSessionEventsOptions,
  getSessionLogsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Badge } from '@/components/ui/badge'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ArrowLeft,
  Clock,
  Globe,
  CheckCircle,
  XCircle,
  AlertCircle,
  Globe as MapPinIcon,
  Users as UserIcon,
  Bug,
  ChevronLeft,
  ChevronRight,
} from 'lucide-react'
import * as React from 'react'
import { useNavigate } from 'react-router-dom'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { DateRange } from 'react-day-picker'
import { subDays } from 'date-fns'

interface SessionDetailProps {
  project: ProjectResponse
  visitorId: number
  sessionId: number
}

export function SessionDetail({
  project,
  visitorId,
  sessionId,
}: SessionDetailProps) {
  const navigate = useNavigate()

  // Date range state
  const [dateRange, setDateRange] = React.useState<DateRange | undefined>(
    () => ({
      from: subDays(new Date(), 30),
      to: new Date(),
    })
  )

  // Pagination state for logs
  const [logsPage, setLogsPage] = React.useState(1)
  const [logsLimit, setLogsLimit] = React.useState(25)

  // Pagination state for events
  const [eventsPage, setEventsPage] = React.useState(1)
  const [eventsLimit, setEventsLimit] = React.useState(25)

  // Format dates for API
  const startDate = dateRange?.from
    ? dateRange.from.toISOString()
    : subDays(new Date(), 30).toISOString()
  const endDate = dateRange?.to
    ? dateRange.to.toISOString()
    : new Date().toISOString()

  const {
    data: sessionDetails,
    isLoading: isLoadingSession,
    error: sessionError,
  } = useQuery({
    ...getSessionDetailsOptions({
      path: {
        session_id: sessionId,
      },
      query: {
        project_id: project.id,
      },
    }),
  })

  const { data: visitorDetails, isLoading: isLoadingVisitor } = useQuery({
    ...getVisitorDetailsOptions({
      path: {
        visitor_id: visitorId,
      },
      query: {
        project_id: project.id,
      },
    }),
  })

  // Separate query for session logs with pagination
  const { data: sessionLogs, isLoading: isLoadingLogs } = useQuery({
    ...getSessionLogsOptions({
      path: {
        session_id: sessionId,
      },
      query: {
        project_id: project.id,
        limit: logsLimit,
        offset: (logsPage - 1) * logsLimit,
        start_date: startDate,
        end_date: endDate,
      },
    }),
  })

  // Separate query for session events with pagination
  const { data: sessionEvents, isLoading: isLoadingEvents } = useQuery({
    ...getSessionEventsOptions({
      path: {
        session_id: sessionId,
      },
      query: {
        project_id: project.id,
        limit: eventsLimit,
        offset: (eventsPage - 1) * eventsLimit,
        start_date: startDate,
        end_date: endDate,
      },
    }),
  })

  // Calculate total pages
  const logsTotalPages = React.useMemo(() => {
    if (!sessionLogs?.total_count) return 0
    return Math.ceil(sessionLogs.total_count / logsLimit)
  }, [sessionLogs, logsLimit])

  const eventsTotalPages = React.useMemo(() => {
    if (!sessionEvents?.total_count) return 0
    return Math.ceil(sessionEvents.total_count / eventsLimit)
  }, [sessionEvents, eventsLimit])

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${Math.round(seconds)}s`
    const minutes = Math.floor(seconds / 60)
    if (minutes < 60) return `${minutes}m ${seconds % 60}s`
    const hours = Math.floor(minutes / 60)
    return `${hours}h ${minutes % 60}m`
  }

  const getStatusIcon = (statusCode: number) => {
    if (statusCode >= 200 && statusCode < 300) {
      return <CheckCircle className="h-4 w-4 text-green-500" />
    } else if (statusCode >= 400 && statusCode < 500) {
      return <XCircle className="h-4 w-4 text-yellow-500" />
    } else if (statusCode >= 500) {
      return <AlertCircle className="h-4 w-4 text-red-500" />
    }
    return <AlertCircle className="h-4 w-4 text-gray-500" />
  }

  const getStatusColor = (statusCode: number) => {
    if (statusCode >= 200 && statusCode < 300) return 'text-green-600'
    if (statusCode >= 300 && statusCode < 400) return 'text-blue-600'
    if (statusCode >= 400 && statusCode < 500) return 'text-yellow-600'
    if (statusCode >= 500) return 'text-red-600'
    return 'text-gray-600'
  }

  const isLoading = isLoadingSession || isLoadingVisitor

  // Reset page when date changes
  React.useEffect(() => {
    setLogsPage(1)
    setEventsPage(1)
  }, [dateRange])

  // Reset page when limit changes
  const handleLogsLimitChange = (value: string) => {
    setLogsLimit(parseInt(value))
    setLogsPage(1)
  }

  const handleEventsLimitChange = (value: string) => {
    setEventsLimit(parseInt(value))
    setEventsPage(1)
  }

  // Helper function to generate pagination button numbers
  const getPaginationPages = (currentPage: number, totalPages: number) => {
    const pageNumbers = []
    const maxButtons = 5
    let startPage = Math.max(1, currentPage - Math.floor(maxButtons / 2))
    const endPage = Math.min(totalPages, startPage + maxButtons - 1)

    if (endPage - startPage < maxButtons - 1) {
      startPage = Math.max(1, endPage - maxButtons + 1)
    }

    for (let i = startPage; i <= endPage; i++) {
      pageNumbers.push(i)
    }

    return pageNumbers
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-4">
        <Button
          variant="ghost"
          size="icon"
          onClick={() =>
            navigate(
              `/projects/${project.slug}/analytics/visitors/${visitorId}`
            )
          }
        >
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div className="flex-1">
          <h2 className="text-2xl font-semibold">Session Details</h2>
          <p className="text-muted-foreground">Session ID: {sessionId}</p>
        </div>
        <DateRangePicker
          date={dateRange}
          onDateChange={setDateRange}
          showTime={true}
        />
      </div>

      {isLoading ? (
        <div className="space-y-4">
          <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
            {[...Array(8)].map((_, i) => (
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
        </div>
      ) : sessionError ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <p className="text-muted-foreground mb-2">
              Failed to load session details
            </p>
            <Button variant="outline" onClick={() => window.location.reload()}>
              Try again
            </Button>
          </CardContent>
        </Card>
      ) : (
        <>
          {/* Session Metrics */}
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-5 gap-4">
            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <Clock className="h-3 w-3" />
                  Duration
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="font-semibold">
                  {sessionDetails &&
                    formatDuration(sessionDetails.duration_seconds)}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription>Bounce</CardDescription>
              </CardHeader>
              <CardContent>
                <Badge
                  variant={sessionDetails?.is_bounced ? 'secondary' : 'default'}
                >
                  {sessionDetails?.is_bounced ? 'Yes' : 'No'}
                </Badge>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription>Engaged</CardDescription>
              </CardHeader>
              <CardContent>
                <Badge
                  variant={sessionDetails?.is_engaged ? 'default' : 'secondary'}
                >
                  {sessionDetails?.is_engaged ? 'Yes' : 'No'}
                </Badge>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription className="flex items-center gap-1">
                  <Globe className="h-3 w-3" />
                  Referrer
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div
                  className="text-sm truncate"
                  title={sessionDetails?.referrer || 'Direct'}
                >
                  {sessionDetails?.referrer || 'Direct'}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader className="pb-2">
                <CardDescription>Started</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="text-sm">
                  {sessionDetails &&
                    format(new Date(sessionDetails.started_at), 'HH:mm:ss')}
                </div>
              </CardContent>
            </Card>
          </div>

          {/* Visitor Information Card */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                {visitorDetails?.is_crawler ? (
                  <Bug className="h-5 w-5" />
                ) : (
                  <UserIcon className="h-5 w-5" />
                )}
                Visitor Information
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-1 md:grid-cols-3 lg:grid-cols-6 gap-4">
                <div>
                  <div className="text-sm text-muted-foreground">
                    Visitor ID
                  </div>
                  <div className="font-mono text-sm mt-1">
                    {visitorId.toString()}
                  </div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">Location</div>
                  <div className="flex items-center gap-1 mt-1">
                    <MapPinIcon className="h-3 w-3" />
                    <span className="text-sm">
                      {visitorDetails?.country || 'Unknown'}
                    </span>
                  </div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">City</div>
                  <div className="text-sm mt-1">
                    {visitorDetails?.city || 'Unknown'}
                  </div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">Type</div>
                  <Badge variant="outline" className="mt-1">
                    {visitorDetails?.is_crawler
                      ? visitorDetails.crawler_name || 'Crawler'
                      : 'Human'}
                  </Badge>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">
                    Total Sessions
                  </div>
                  <div className="text-sm font-medium mt-1">
                    {visitorDetails?.total_sessions}
                  </div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground">
                    Total Page Views
                  </div>
                  <div className="text-sm font-medium mt-1">
                    {visitorDetails?.total_page_views}
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Path Information */}
          <Card>
            <CardHeader>
              <CardTitle>Session Path</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                <div>
                  <div className="text-sm text-muted-foreground mb-1">
                    Entry Page
                  </div>
                  <div className="font-mono text-sm bg-muted p-2 rounded">
                    {sessionDetails?.entry_path || '-'}
                  </div>
                </div>
                <div>
                  <div className="text-sm text-muted-foreground mb-1">
                    Exit Page
                  </div>
                  <div className="font-mono text-sm bg-muted p-2 rounded">
                    {sessionDetails?.exit_path || '-'}
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Tabs for Events and Request Logs */}
          <Tabs defaultValue="requests" className="w-full">
            <TabsList className="grid w-full grid-cols-2">
              <TabsTrigger value="requests">
                Request Logs ({sessionLogs?.total_count || 0})
              </TabsTrigger>
              <TabsTrigger value="events">
                Events ({sessionEvents?.total_count || 0})
              </TabsTrigger>
            </TabsList>

            <TabsContent value="requests" className="mt-6">
              <Card>
                <CardHeader>
                  <div className="flex items-center justify-between">
                    <div>
                      <CardTitle>Request Logs</CardTitle>
                      <CardDescription>
                        All HTTP requests made during this session
                      </CardDescription>
                    </div>
                    <Select
                      value={logsLimit.toString()}
                      onValueChange={handleLogsLimitChange}
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
                  {isLoadingLogs ? (
                    <div className="space-y-2">
                      {[...Array(5)].map((_, i) => (
                        <Skeleton key={i} className="h-12 w-full" />
                      ))}
                    </div>
                  ) : !sessionLogs?.logs || sessionLogs.logs.length === 0 ? (
                    <div className="text-center py-8 text-muted-foreground">
                      No request logs found
                    </div>
                  ) : (
                    <>
                      <Table>
                        <TableHeader>
                          <TableRow>
                            <TableHead className="w-[100px]">Time</TableHead>
                            <TableHead className="w-[80px]">Method</TableHead>
                            <TableHead className="w-[80px]">Status</TableHead>
                            <TableHead>Path</TableHead>
                            <TableHead className="w-[100px]">
                              Response Time
                            </TableHead>
                            <TableHead>Referrer</TableHead>
                          </TableRow>
                        </TableHeader>
                        <TableBody>
                          {sessionLogs.logs.map((log: any) => (
                            <TableRow
                              key={log.id}
                              className="cursor-pointer hover:bg-muted/50"
                              onClick={() =>
                                navigate(
                                  `/projects/${project.slug}/logs/${log.id}`
                                )
                              }
                            >
                              <TableCell className="text-sm">
                                {format(
                                  new Date(log.created_at || log.finished_at),
                                  'HH:mm:ss'
                                )}
                              </TableCell>
                              <TableCell>
                                <Badge variant="outline" className="font-mono">
                                  {log.method}
                                </Badge>
                              </TableCell>
                              <TableCell>
                                <div className="flex items-center gap-1">
                                  {getStatusIcon(log.status_code)}
                                  <span
                                    className={cn(
                                      'font-mono text-sm',
                                      getStatusColor(log.status_code)
                                    )}
                                  >
                                    {log.status_code}
                                  </span>
                                </div>
                              </TableCell>
                              <TableCell className="font-mono text-sm max-w-[300px] truncate">
                                {log.path || log.request_path}
                              </TableCell>
                              <TableCell className="text-sm">
                                {log.response_time_ms
                                  ? `${log.response_time_ms}ms`
                                  : log.elapsed_time
                                    ? `${log.elapsed_time}ms`
                                    : '-'}
                              </TableCell>
                              <TableCell className="text-sm max-w-[200px] truncate">
                                {log.referrer || '-'}
                              </TableCell>
                            </TableRow>
                          ))}
                        </TableBody>
                      </Table>

                      {/* Pagination */}
                      {logsTotalPages > 1 && (
                        <div className="flex items-center justify-between mt-6">
                          <div className="text-sm text-muted-foreground">
                            Showing {(logsPage - 1) * logsLimit + 1} to{' '}
                            {Math.min(
                              logsPage * logsLimit,
                              sessionLogs.total_count
                            )}{' '}
                            of {sessionLogs.total_count} logs
                          </div>
                          <div className="flex items-center gap-2">
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                setLogsPage((p) => Math.max(1, p - 1))
                              }
                              disabled={logsPage === 1}
                            >
                              <ChevronLeft className="h-4 w-4" />
                              Previous
                            </Button>
                            <div className="flex items-center gap-1">
                              {getPaginationPages(logsPage, logsTotalPages).map(
                                (pageNum) => (
                                  <Button
                                    key={pageNum}
                                    variant={
                                      pageNum === logsPage
                                        ? 'default'
                                        : 'outline'
                                    }
                                    size="sm"
                                    onClick={() => setLogsPage(pageNum)}
                                    className="w-10"
                                  >
                                    {pageNum}
                                  </Button>
                                )
                              )}
                            </div>
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                setLogsPage((p) =>
                                  Math.min(logsTotalPages, p + 1)
                                )
                              }
                              disabled={logsPage === logsTotalPages}
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

            <TabsContent value="events" className="mt-6">
              <Card>
                <CardHeader>
                  <div className="flex items-center justify-between">
                    <div>
                      <CardTitle>Events</CardTitle>
                      <CardDescription>
                        Custom events tracked during this session
                      </CardDescription>
                    </div>
                    <Select
                      value={eventsLimit.toString()}
                      onValueChange={handleEventsLimitChange}
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
                  {isLoadingEvents ? (
                    <div className="space-y-2">
                      {[...Array(5)].map((_, i) => (
                        <Skeleton key={i} className="h-12 w-full" />
                      ))}
                    </div>
                  ) : !sessionEvents?.events ||
                    sessionEvents.events.length === 0 ? (
                    <div className="text-center py-8 text-muted-foreground">
                      No events tracked
                    </div>
                  ) : (
                    <>
                      <Table>
                        <TableHeader>
                          <TableRow>
                            <TableHead className="w-[100px]">Time</TableHead>
                            <TableHead>Event Name</TableHead>
                            <TableHead>Path</TableHead>
                            <TableHead>Query</TableHead>
                            <TableHead>Event Data</TableHead>
                          </TableRow>
                        </TableHeader>
                        <TableBody>
                          {sessionEvents.events.map((event: any) => (
                            <TableRow key={event.id}>
                              <TableCell className="text-sm">
                                {format(
                                  new Date(
                                    event.occurred_at || event.created_at
                                  ),
                                  'HH:mm:ss'
                                )}
                              </TableCell>
                              <TableCell>
                                <Badge>{event.event_name}</Badge>
                              </TableCell>
                              <TableCell className="font-mono text-sm max-w-[250px] truncate">
                                {event.request_path || event.path || '-'}
                              </TableCell>
                              <TableCell className="font-mono text-sm max-w-[150px] truncate">
                                {event.request_query || event.query || '-'}
                              </TableCell>
                              <TableCell className="text-sm max-w-[300px]">
                                <code className="text-xs bg-muted p-1 rounded truncate block">
                                  {JSON.stringify(
                                    event.event_data || event.data || {}
                                  )}
                                </code>
                              </TableCell>
                            </TableRow>
                          ))}
                        </TableBody>
                      </Table>

                      {/* Pagination */}
                      {eventsTotalPages > 1 && (
                        <div className="flex items-center justify-between mt-6">
                          <div className="text-sm text-muted-foreground">
                            Showing {(eventsPage - 1) * eventsLimit + 1} to{' '}
                            {Math.min(
                              eventsPage * eventsLimit,
                              sessionEvents.total_count
                            )}{' '}
                            of {sessionEvents.total_count} events
                          </div>
                          <div className="flex items-center gap-2">
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                setEventsPage((p) => Math.max(1, p - 1))
                              }
                              disabled={eventsPage === 1}
                            >
                              <ChevronLeft className="h-4 w-4" />
                              Previous
                            </Button>
                            <div className="flex items-center gap-1">
                              {getPaginationPages(
                                eventsPage,
                                eventsTotalPages
                              ).map((pageNum) => (
                                <Button
                                  key={pageNum}
                                  variant={
                                    pageNum === eventsPage
                                      ? 'default'
                                      : 'outline'
                                  }
                                  size="sm"
                                  onClick={() => setEventsPage(pageNum)}
                                  className="w-10"
                                >
                                  {pageNum}
                                </Button>
                              ))}
                            </div>
                            <Button
                              variant="outline"
                              size="sm"
                              onClick={() =>
                                setEventsPage((p) =>
                                  Math.min(eventsTotalPages, p + 1)
                                )
                              }
                              disabled={eventsPage === eventsTotalPages}
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
          </Tabs>
        </>
      )}
    </div>
  )
}
