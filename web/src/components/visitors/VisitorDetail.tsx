import {
  enrichVisitorMutation,
  getVisitorDetailsOptions,
  getVisitorDetailsQueryKey,
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
import { CopyButton } from '@/components/ui/copy-button'
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
import { Textarea } from '@/components/ui/textarea'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
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
  Loader2,
  Monitor,
  Pencil,
  PlayCircle,
  Plus,
  Smartphone,
  Users as UserIcon,
} from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'

interface VisitorDetailProps {
  project: ProjectResponse
  visitorId: number
}

// Helper function to parse user agent for browser/OS/device info
function parseUserAgent(userAgent: string | null | undefined) {
  if (!userAgent) {
    return {
      browser: 'Unknown',
      os: 'Unknown',
      device: 'Unknown',
      deviceType: 'desktop' as const,
    }
  }

  // Detect browser
  let browser = 'Unknown'
  if (userAgent.includes('Firefox')) browser = 'Firefox'
  else if (userAgent.includes('Edg/')) browser = 'Edge'
  else if (userAgent.includes('Chrome')) browser = 'Chrome'
  else if (userAgent.includes('Safari') && !userAgent.includes('Chrome'))
    browser = 'Safari'
  else if (userAgent.includes('Opera') || userAgent.includes('OPR'))
    browser = 'Opera'

  // Detect OS
  let os = 'Unknown'
  if (userAgent.includes('Windows')) os = 'Windows'
  else if (userAgent.includes('Mac OS X')) {
    const match = userAgent.match(/Mac OS X ([\d_]+)/)
    os = match ? `macOS ${match[1].replace(/_/g, '.')}` : 'macOS'
  } else if (userAgent.includes('Linux')) os = 'Linux'
  else if (userAgent.includes('Android')) os = 'Android'
  else if (userAgent.includes('iOS') || userAgent.includes('iPhone')) os = 'iOS'

  // Detect device type
  let deviceType: 'mobile' | 'tablet' | 'desktop' = 'desktop'
  let device = 'Desktop'

  if (
    userAgent.includes('Mobile') ||
    userAgent.includes('iPhone') ||
    userAgent.includes('Android')
  ) {
    deviceType = 'mobile'
    device = 'Mobile'
  } else if (userAgent.includes('iPad') || userAgent.includes('Tablet')) {
    deviceType = 'tablet'
    device = 'Tablet'
  }

  return { browser, os, device, deviceType }
}

// Component for displaying browser and device information
function BrowserDeviceInfo({
  userAgent,
}: {
  userAgent: string | null | undefined
}) {
  const userAgentInfo = parseUserAgent(userAgent)

  return (
    <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
      {/* Browser Card */}
      <Card>
        <CardHeader className="pb-3">
          <CardDescription className="flex items-center gap-1.5">
            <Monitor className="h-4 w-4" />
            Browser
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="text-lg font-semibold">{userAgentInfo.browser}</div>
        </CardContent>
      </Card>

      {/* Operating System Card */}
      <Card>
        <CardHeader className="pb-3">
          <CardDescription className="flex items-center gap-1.5">
            <Monitor className="h-4 w-4" />
            Operating System
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="text-lg font-semibold">{userAgentInfo.os}</div>
        </CardContent>
      </Card>

      {/* Device Type Card */}
      <Card>
        <CardHeader className="pb-3">
          <CardDescription className="flex items-center gap-1.5">
            {userAgentInfo.deviceType === 'mobile' ? (
              <Smartphone className="h-4 w-4" />
            ) : (
              <Monitor className="h-4 w-4" />
            )}
            Device
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="text-lg font-semibold">{userAgentInfo.device}</div>
        </CardContent>
      </Card>
    </div>
  )
}

export function VisitorDetail({ project, visitorId }: VisitorDetailProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [searchParams, setSearchParams] = useSearchParams()
  const [page, setPage] = React.useState(1)
  const [limit, setLimit] = React.useState(25)
  const [isEnrichDialogOpen, setIsEnrichDialogOpen] = React.useState(false)
  const [enrichJsonValue, setEnrichJsonValue] = React.useState('')
  const [enrichJsonError, setEnrichJsonError] = React.useState<string | null>(null)

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

  // Mutation for enriching visitor data
  const enrichMutation = useMutation({
    ...enrichVisitorMutation(),
    onSuccess: () => {
      toast.success('Visitor data enriched successfully')
      setIsEnrichDialogOpen(false)
      setEnrichJsonValue('')
      setEnrichJsonError(null)
      // Invalidate the visitor details query to refetch
      queryClient.invalidateQueries({
        queryKey: getVisitorDetailsQueryKey({
          path: { visitor_id: visitorId },
          query: { project_id: project.id },
        }),
      })
    },
    onError: (error: any) => {
      toast.error('Failed to enrich visitor data', {
        description: error?.message || 'Please try again',
      })
    },
  })

  // Handle opening the enrich dialog
  const handleOpenEnrichDialog = () => {
    // Pre-populate with existing custom_data if available
    if (visitorDetails?.custom_data && Object.keys(visitorDetails.custom_data).length > 0) {
      setEnrichJsonValue(JSON.stringify(visitorDetails.custom_data, null, 2))
    } else {
      setEnrichJsonValue('{\n  \n}')
    }
    setEnrichJsonError(null)
    setIsEnrichDialogOpen(true)
  }

  // Handle submitting the enrich form
  const handleEnrichSubmit = () => {
    try {
      const parsedData = JSON.parse(enrichJsonValue)
      if (typeof parsedData !== 'object' || parsedData === null || Array.isArray(parsedData)) {
        setEnrichJsonError('Custom data must be a JSON object (not an array or primitive)')
        return
      }
      setEnrichJsonError(null)

      // Use visitor_id GUID if available, otherwise use numeric ID
      const visitorIdToUse = visitorDetails?.visitor_id || visitorId.toString()

      enrichMutation.mutate({
        path: { visitor_id: visitorIdToUse },
        query: { project_id: project.id },
        body: { custom_data: parsedData },
      })
    } catch {
      setEnrichJsonError('Invalid JSON format')
    }
  }

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
        {visitorDetails && (
          <Button
            variant="outline"
            size="sm"
            onClick={handleOpenEnrichDialog}
          >
            <Pencil className="h-4 w-4 mr-2" />
            Enrich Visitor
          </Button>
        )}
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
          {/* Visitor Info Cards - Redesigned with better layout */}
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
            {/* Location Card */}
            <Card>
              <CardHeader className="pb-3">
                <CardDescription className="flex items-center gap-1.5">
                  <MapPinIcon className="h-4 w-4" />
                  Location
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="text-xl font-semibold">
                  {visitorDetails?.country || 'Unknown'}
                </div>
                {visitorDetails?.city && (
                  <div className="text-sm text-muted-foreground mt-1">
                    {visitorDetails.city}
                  </div>
                )}
              </CardContent>
            </Card>

            {/* First Seen Card */}
            <Card>
              <CardHeader className="pb-3">
                <CardDescription className="flex items-center gap-1.5">
                  <Calendar className="h-4 w-4" />
                  First Seen
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="text-xl font-semibold">
                  {visitorDetails &&
                    format(new Date(visitorDetails.first_seen), 'MMM d, yyyy')}
                </div>
                <div className="text-sm text-muted-foreground mt-1">
                  {visitorDetails &&
                    format(new Date(visitorDetails.first_seen), 'HH:mm:ss')}
                </div>
              </CardContent>
            </Card>

            {/* Last Activity Card */}
            <Card>
              <CardHeader className="pb-3">
                <CardDescription className="flex items-center gap-1.5">
                  <Clock className="h-4 w-4" />
                  Last Activity
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="text-xl font-semibold">
                  {visitorDetails &&
                    format(new Date(visitorDetails.last_seen), 'MMM d, yyyy')}
                </div>
                <div className="text-sm text-muted-foreground mt-1">
                  {visitorDetails &&
                    format(new Date(visitorDetails.last_seen), 'HH:mm:ss')}
                </div>
              </CardContent>
            </Card>

            {/* Type Card */}
            <Card>
              <CardHeader className="pb-3">
                <CardDescription className="flex items-center gap-1.5">
                  {visitorDetails?.is_crawler ? (
                    <Bug className="h-4 w-4" />
                  ) : (
                    <UserIcon className="h-4 w-4" />
                  )}
                  Visitor Type
                </CardDescription>
              </CardHeader>
              <CardContent>
                <Badge
                  variant={visitorDetails?.is_crawler ? 'secondary' : 'default'}
                  className="text-base px-3 py-1"
                >
                  {visitorDetails?.is_crawler
                    ? visitorDetails.crawler_name || 'Crawler'
                    : 'Human Visitor'}
                </Badge>
              </CardContent>
            </Card>
          </div>

          {/* Browser & Device Information - New Section */}
          <BrowserDeviceInfo userAgent={visitorDetails?.user_agent} />

          {/* User Agent - Full Display with Copy */}
          <Card>
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between">
                <CardTitle className="text-base">User Agent</CardTitle>
                <CopyButton
                  value={visitorDetails?.user_agent || ''}
                  className="h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
                />
              </div>
            </CardHeader>
            <CardContent>
              <div className="text-sm font-mono bg-muted p-3 rounded-md break-all">
                {visitorDetails?.user_agent || 'Unknown'}
              </div>
            </CardContent>
          </Card>

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
                  <div className="overflow-x-auto">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead className="w-[100px]">
                            Session ID
                          </TableHead>
                          <TableHead className="w-[180px]">Started</TableHead>
                          <TableHead className="w-[100px]">Duration</TableHead>
                          <TableHead className="w-[80px] text-center">
                            Pages
                          </TableHead>
                          <TableHead className="w-[80px] text-center">
                            Events
                          </TableHead>
                          <TableHead className="min-w-[200px]">
                            Entry Page
                          </TableHead>
                          <TableHead className="min-w-[200px]">
                            Exit Page
                          </TableHead>
                          <TableHead className="w-[100px] text-center">
                            Bounce
                          </TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {sessions.sessions.map((session) => (
                          <TableRow
                            key={session.session_id}
                            className="cursor-pointer hover:bg-muted/50 transition-colors"
                            onClick={() =>
                              navigate(
                                `/projects/${project.slug}/analytics/visitors/${visitorId}/sessions/${session.session_id.toString()}`
                              )
                            }
                          >
                            <TableCell className="font-mono text-xs">
                              {session.session_id}
                            </TableCell>
                            <TableCell>
                              {session.started_at &&
                              !isNaN(Date.parse(session.started_at)) ? (
                                <div className="flex flex-col">
                                  <span className="text-sm font-medium">
                                    {format(
                                      new Date(session.started_at),
                                      'MMM d, yyyy'
                                    )}
                                  </span>
                                  <span className="text-xs text-muted-foreground">
                                    {format(
                                      new Date(session.started_at),
                                      'HH:mm:ss'
                                    )}
                                  </span>
                                </div>
                              ) : (
                                <span className="text-sm text-muted-foreground">
                                  -
                                </span>
                              )}
                            </TableCell>
                            <TableCell className="text-sm font-medium">
                              {formatDuration(session.duration_seconds || 0)}
                            </TableCell>
                            <TableCell className="text-center">
                              <Badge variant="secondary" className="text-sm">
                                {session.page_views || 0}
                              </Badge>
                            </TableCell>
                            <TableCell className="text-center">
                              <Badge variant="outline" className="text-sm">
                                {session.events_count}
                              </Badge>
                            </TableCell>
                            <TableCell className="max-w-[300px]">
                              {session.entry_path ? (
                                <div
                                  className="text-sm truncate font-mono"
                                  title={session.entry_path}
                                >
                                  {session.entry_path}
                                </div>
                              ) : (
                                <span className="text-sm text-muted-foreground">
                                  -
                                </span>
                              )}
                            </TableCell>
                            <TableCell className="max-w-[300px]">
                              {session.exit_path ? (
                                <div
                                  className="text-sm truncate font-mono"
                                  title={session.exit_path}
                                >
                                  {session.exit_path}
                                </div>
                              ) : (
                                <span className="text-sm text-muted-foreground">
                                  -
                                </span>
                              )}
                            </TableCell>
                            <TableCell className="text-center">
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
                  </div>

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
                          {getPaginationPages(page, totalPages).map(
                            (pageNum) => (
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
                            )
                          )}
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

      {/* Enrich Visitor Dialog */}
      <Dialog open={isEnrichDialogOpen} onOpenChange={setIsEnrichDialogOpen}>
        <DialogContent className="sm:max-w-[600px]">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Pencil className="h-5 w-5" />
              Enrich Visitor Data
            </DialogTitle>
            <DialogDescription>
              Add or update custom data for this visitor. The data will be merged with existing custom data.
              Enter valid JSON object format.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="custom_data">Custom Data (JSON)</Label>
              <Textarea
                id="custom_data"
                value={enrichJsonValue}
                onChange={(e) => {
                  setEnrichJsonValue(e.target.value)
                  setEnrichJsonError(null)
                }}
                placeholder='{"email": "user@example.com", "name": "John Doe", "plan": "premium"}'
                className="font-mono text-sm min-h-[200px]"
              />
              {enrichJsonError && (
                <p className="text-sm text-destructive">{enrichJsonError}</p>
              )}
            </div>
            <div className="text-sm text-muted-foreground">
              <p className="font-medium mb-1">Examples:</p>
              <ul className="list-disc list-inside space-y-1">
                <li><code className="bg-muted px-1 rounded">{`{"email": "user@example.com"}`}</code></li>
                <li><code className="bg-muted px-1 rounded">{`{"company": "Acme Inc", "role": "admin"}`}</code></li>
                <li><code className="bg-muted px-1 rounded">{`{"userId": 12345, "isPremium": true}`}</code></li>
              </ul>
            </div>
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setIsEnrichDialogOpen(false)}
              disabled={enrichMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              onClick={handleEnrichSubmit}
              disabled={enrichMutation.isPending || !enrichJsonValue.trim()}
            >
              {enrichMutation.isPending ? (
                <>
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  Saving...
                </>
              ) : (
                <>
                  <Plus className="h-4 w-4 mr-2" />
                  Enrich Visitor
                </>
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
