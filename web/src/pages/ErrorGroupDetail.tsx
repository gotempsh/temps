import {
  ErrorEventResponse,
  ProjectResponse,
  RouteUserWithRoles,
  UserResponse,
} from '@/api/client'
import {
  getEnvironmentsOptions,
  getErrorGroupOptions,
  listErrorEventsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AutofixButton } from '@/components/autofixer/AutofixButton'
import { SentryEventDetail } from '@/components/error-tracking/SentryEventDetail'
import { SentryListItem } from '@/components/error-tracking/SentryListItem'
import { updateErrorGroupMutation } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useAssistantPageContext } from '@/components/ai/AiAssistantContext'
import { useAuth } from '@/contexts/AuthContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import { extractSentryEvent } from '@/lib/sentry-utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  Check,
  EyeOff,
  MoreVertical,
  RotateCcw,
  UserRound,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router'

export function ErrorGroupDetail({ project }: { project: ProjectResponse }) {
  const { projectSlug, errorGroupId } = useParams<{
    projectSlug: string
    errorGroupId: string
  }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  const [selectedTab, setSelectedTab] = useState('overview')

  // Fetch error group details
  const {
    data: errorGroup,
    isLoading: isLoadingGroup,
    error: errorGroupError,
    refetch: refetchErrorGroup,
  } = useQuery({
    ...getErrorGroupOptions({
      path: { group_id: parseInt(errorGroupId!), project_id: project.id },
    }),
    enabled: !!errorGroupId,
  })

  // Fetch error events for this group
  const { data: errorEvents, isLoading: isLoadingEvents } = useQuery({
    ...listErrorEventsOptions({
      path: { group_id: parseInt(errorGroupId!), project_id: project.id },
      query: {
        page_size: 100,
        page: 1,
      },
    }),
    enabled: !!errorGroupId,
  })

  const statusMutation = useMutation({
    ...updateErrorGroupMutation(),
    meta: { errorTitle: 'Failed to update error status' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getErrorGroupOptions({
          path: { group_id: parseInt(errorGroupId!), project_id: project.id },
        }).queryKey,
      })
    },
  })

  const updateStatus = (status: string) => {
    statusMutation.mutate({
      path: { group_id: parseInt(errorGroupId!), project_id: project.id },
      body: { status },
    })
  }

  const { user: currentUser } = useAuth()

  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
  })

  // No per-project members endpoint exists yet — same instance-wide user
  // source TeamDetail's AddMemberDialog uses to resolve `assigned_to` (an
  // email/username string) to a display name + avatar.
  const { data: usersData } = useQuery(
    listUsersOptions({ query: { include_deleted: false } })
  )
  const userByIdentity = useMemo(() => {
    const map = new Map<string, NonNullable<typeof usersData>[number]>()
    for (const u of usersData ?? []) {
      if (u.user.email) map.set(u.user.email.toLowerCase(), u)
      if (u.user.username) map.set(u.user.username.toLowerCase(), u)
    }
    return map
  }, [usersData])

  const assigneeMutation = useMutation({
    ...updateErrorGroupMutation(),
    meta: { errorTitle: 'Failed to update assignee' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getErrorGroupOptions({
          path: { group_id: parseInt(errorGroupId!), project_id: project.id },
        }).queryKey,
      })
    },
  })
  const assign = (assignedTo: string) =>
    assigneeMutation.mutate({
      path: { group_id: parseInt(errorGroupId!), project_id: project.id },
      body: {
        status: (errorGroup as { status?: string })?.status ?? 'unresolved',
        assigned_to: assignedTo,
      },
    })

  usePageTitle(errorGroup ? `Error: ${errorGroup.title}` : 'Error Details')

  useEffect(() => {
    if (errorGroup && projectSlug) {
      setBreadcrumbs([
        { label: 'Projects', href: '/projects' },
        { label: projectSlug, href: `/projects/${projectSlug}` },
        { label: 'Error Tracking', href: `/projects/${projectSlug}/errors` },
        { label: errorGroup.title || 'Error Details' },
      ])
    }
  }, [setBreadcrumbs, errorGroup, projectSlug])

  // Tell the assistant which error the user is looking at.
  const assistantContext = errorGroup
    ? [
      'The user is viewing an error group (error tracking) in the Temps console.',
      `Project: "${project.name}" (slug: ${project.slug}, id: ${project.id}).`,
      `Error group #${errorGroupId}: "${errorGroup.title}" (type: ${errorGroup.error_type ?? 'unknown'}).`,
      `Seen ${errorGroup.total_count} time(s); first ${errorGroup.first_seen}, last ${errorGroup.last_seen}.`,
      'Fetch details via the temps CLI: `error-tracking get_error_group --group_id` and `list_error_events --group_id`.',
    ].join('\n')
    : null
  useAssistantPageContext(assistantContext, 'this error')

  const getSeverityColor = (level: string) => {
    switch (level?.toLowerCase()) {
      case 'error':
      case 'fatal':
        return 'text-red-600 bg-red-100 dark:bg-red-900/20'
      case 'warning':
        return 'text-yellow-600 bg-yellow-100 dark:bg-yellow-900/20'
      case 'info':
        return 'text-blue-600 bg-blue-100 dark:bg-blue-900/20'
      default:
        return 'text-gray-600 bg-gray-100 dark:bg-gray-900/20'
    }
  }

  if (isLoadingGroup || isLoadingEvents) {
    return (
      <div className="space-y-6 p-4 sm:p-6">
        <div className="flex items-center justify-between">
          <Skeleton className="h-8 w-64" />
          <div className="flex gap-2">
            <Skeleton className="h-10 w-24" />
            <Skeleton className="h-10 w-24" />
          </div>
        </div>
        <Card>
          <CardHeader>
            <Skeleton className="h-6 w-32" />
            <Skeleton className="h-4 w-48" />
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              <Skeleton className="h-20" />
              <Skeleton className="h-20" />
              <Skeleton className="h-20" />
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  const isNotFound =
    (errorGroupError as any)?.status === 404 ||
    (errorGroupError as any)?.title === 'Not Found'

  if (!errorGroup && errorGroupError && !isNotFound) {
    return (
      <div className="flex flex-col items-center justify-center gap-4 min-h-[400px]">
        <AlertCircle className="h-8 w-8 text-destructive" />
        <div className="text-center">
          <h2 className="text-lg font-semibold">Failed to load error group</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {errorGroupError instanceof Error
              ? errorGroupError.message
              : 'An unexpected error occurred. Please try again.'}
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={() => void refetchErrorGroup()}>
            <RotateCcw className="mr-2 h-4 w-4" />
            Retry
          </Button>
          <Button variant="ghost" onClick={() => navigate(`/projects/${project.slug}/errors`)}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Error Tracking
          </Button>
        </div>
      </div>
    )
  }

  if (!errorGroup) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>Error group not found</AlertDescription>
        </Alert>
      </div>
    )
  }

  const latestEvent = errorEvents?.data?.[0] as ErrorEventResponse | undefined
  const sentryEvent = latestEvent ? extractSentryEvent(latestEvent.data) : null

  return (
    <div className="p-4 sm:p-6">
      {/* Error Title Bar with Back Button */}
      <div className="mb-6 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/projects/${project.slug}/errors`)}
            className="-ml-2 shrink-0"
            aria-label="Back to error tracking"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <Badge
            className={cn(getSeverityColor(errorGroup.error_type || 'error'))}
          >
            {errorGroup.error_type || 'error'}
          </Badge>
          {(errorGroup as any).status &&
            (errorGroup as any).status !== 'unresolved' && (
              <Badge
                variant={
                  (errorGroup as any).status === 'resolved'
                    ? 'default'
                    : 'secondary'
                }
                className="text-xs"
              >
                {(errorGroup as any).status}
              </Badge>
            )}
          {errorGroup.environment_id != null && (
            <Badge variant="outline" className="capitalize text-xs">
              {environments?.find((e) => e.id === errorGroup.environment_id)
                ?.name ?? 'Unknown environment'}
            </Badge>
          )}
        </div>

        <h1 className="text-lg sm:text-xl md:text-2xl font-semibold break-words">
          {errorGroup.title}
        </h1>

        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex flex-col gap-1 text-sm text-muted-foreground sm:flex-row sm:flex-wrap sm:items-center sm:gap-x-2 sm:gap-y-1">
            <span>{errorGroup.total_count || 0} occurrences</span>
            <span className="hidden sm:inline">•</span>
            <span>
              First seen <TimeAgo date={errorGroup.first_seen} />
            </span>
            <span className="hidden sm:inline">•</span>
            <span>
              Last seen <TimeAgo date={errorGroup.last_seen} />
            </span>
          </div>

          <div className="flex items-center gap-2">
            <AssigneeControl
              assignedTo={errorGroup.assigned_to}
              userByIdentity={userByIdentity}
              currentUser={currentUser}
              users={usersData}
              onAssign={assign}
              isPending={assigneeMutation.isPending}
            />
          </div>

          {/* Desktop: full action buttons */}
          <div className="hidden gap-2 sm:flex sm:flex-wrap">
            {(errorGroup as any).status !== 'resolved' && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => updateStatus('resolved')}
                disabled={statusMutation.isPending}
              >
                <Check className="h-4 w-4 mr-1.5" />
                Resolve
              </Button>
            )}
            {(errorGroup as any).status !== 'ignored' &&
              (errorGroup as any).status !== 'resolved' && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => updateStatus('ignored')}
                  disabled={statusMutation.isPending}
                >
                  <EyeOff className="h-4 w-4 mr-1.5" />
                  Ignore
                </Button>
              )}
            {((errorGroup as any).status === 'resolved' ||
              (errorGroup as any).status === 'ignored') && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => updateStatus('unresolved')}
                  disabled={statusMutation.isPending}
                >
                  <RotateCcw className="h-4 w-4 mr-1.5" />
                  Unresolve
                </Button>
              )}
          </div>

          {/* Mobile: actions collapsed behind a kebab menu */}
          <div className="sm:hidden">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  size="icon"
                  disabled={statusMutation.isPending}
                  aria-label="Error actions"
                >
                  <MoreVertical className="h-4 w-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {(errorGroup as any).status !== 'resolved' && (
                  <DropdownMenuItem onClick={() => updateStatus('resolved')}>
                    <Check className="mr-2 h-4 w-4" />
                    Resolve
                  </DropdownMenuItem>
                )}
                {(errorGroup as any).status !== 'ignored' &&
                  (errorGroup as any).status !== 'resolved' && (
                    <DropdownMenuItem onClick={() => updateStatus('ignored')}>
                      <EyeOff className="mr-2 h-4 w-4" />
                      Ignore
                    </DropdownMenuItem>
                  )}
                {((errorGroup as any).status === 'resolved' ||
                  (errorGroup as any).status === 'ignored') && (
                    <DropdownMenuItem onClick={() => updateStatus('unresolved')}>
                      <RotateCcw className="mr-2 h-4 w-4" />
                      Unresolve
                    </DropdownMenuItem>
                  )}
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>
      </div>

      {/* Autofix section — always rendered. When the repo/provider/sandbox
          aren't set up yet the card onboards the user instead of vanishing. */}
      <div className="mb-6">
        <AutofixButton
          projectId={project.id}
          projectSlug={project.slug}
          errorGroupId={parseInt(errorGroupId!)}
          git={{
            connected: !!project.git_provider_connection_id,
            hasRepo: !!project.repo_owner && !!project.repo_name,
            label:
              project.repo_owner && project.repo_name
                ? `${project.repo_owner}/${project.repo_name}`
                : undefined,
          }}
        />
      </div>

      {/* Tabs for different views */}
      <Tabs value={selectedTab} onValueChange={setSelectedTab}>
        <TabsList className="grid w-full max-w-[400px] grid-cols-2">
          <TabsTrigger value="overview">Overview</TabsTrigger>
          <TabsTrigger value="events">
            Events ({errorEvents?.data?.length || 0})
          </TabsTrigger>
        </TabsList>

        {/* Overview Tab - Show Sentry Event Detail or Fallback */}
        <TabsContent value="overview" className="space-y-4 mt-6">
          {sentryEvent ? (
            <SentryEventDetail
              event={sentryEvent}
              showRawData={false}
              showHeader={false}
            />
          ) : latestEvent ? (
            <Card>
              <CardHeader>
                <CardTitle>Latest Event</CardTitle>
                <CardDescription>
                  {format(new Date(latestEvent.timestamp), 'PPpp')}
                </CardDescription>
              </CardHeader>
              <CardContent>
                <ScrollArea className="h-[400px]">
                  <pre className="text-xs whitespace-pre-wrap break-all">
                    {(() => {
                      // Show only meaningful fields, not raw stream-json blobs
                      const summary: Record<string, unknown> = {
                        id: latestEvent.id,
                        timestamp: latestEvent.timestamp,
                      }
                      const d = latestEvent.data as
                        Record<string, unknown> | undefined
                      if (d && typeof d === 'object') {
                        for (const key of [
                          'message',
                          'request',
                          'user',
                          'tags',
                          'contexts',
                          'environment',
                          'release',
                        ]) {
                          if (key in d && d[key]) summary[key] = d[key]
                        }
                      }
                      return JSON.stringify(summary, null, 2)
                    })()}
                  </pre>
                </ScrollArea>
              </CardContent>
            </Card>
          ) : (
            <Alert>
              <AlertDescription>No event data available</AlertDescription>
            </Alert>
          )}
        </TabsContent>

        {/* Events Tab */}
        <TabsContent value="events" className="mt-6">
          <ScrollArea className="h-[calc(100vh-300px)]">
            <div className="space-y-3">
              {errorEvents?.data?.map((event) => {
                const eventSentry = extractSentryEvent(event.data)
                return eventSentry ? (
                  <SentryListItem
                    key={event.id}
                    event={eventSentry}
                    onClick={() =>
                      navigate(
                        `/projects/${project.slug}/errors/${errorGroupId}/event/${event.id}`
                      )
                    }
                  />
                ) : (
                  <Card
                    key={event.id}
                    className="cursor-pointer hover:bg-accent/50 transition-colors"
                    onClick={() =>
                      navigate(
                        `/projects/${project.slug}/errors/${errorGroupId}/event/${event.id}`
                      )
                    }
                  >
                    <CardContent className="p-4">
                      <div className="text-sm text-muted-foreground">
                        {format(new Date(event.timestamp), 'PPpp')}
                      </div>
                      <div className="text-xs text-muted-foreground mt-1">
                        Event ID: {event.id}
                      </div>
                    </CardContent>
                  </Card>
                )
              })}
            </div>
          </ScrollArea>
        </TabsContent>
      </Tabs>
    </div>
  )
}

function AssigneeControl({
  assignedTo,
  userByIdentity,
  currentUser,
  users,
  onAssign,
  isPending,
}: {
  assignedTo?: string | null
  userByIdentity: Map<string, RouteUserWithRoles>
  currentUser: UserResponse | null
  users?: RouteUserWithRoles[]
  onAssign: (assignedTo: string) => void
  isPending: boolean
}) {
  const assignedUser = assignedTo
    ? userByIdentity.get(assignedTo.toLowerCase())
    : undefined
  const label = assignedTo
    ? (assignedUser?.user.name ?? assignedTo)
    : 'Unassigned'
  const initials = (
    assignedUser?.user.username ||
    assignedUser?.user.name ||
    assignedTo ||
    '?'
  )
    .slice(0, 2)
    .toUpperCase()
  const isAssignedToMe =
    !!assignedTo &&
    !!currentUser?.email &&
    assignedTo.toLowerCase() === currentUser.email.toLowerCase()

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          disabled={isPending}
          className="gap-2"
        >
          {assignedTo ? (
            <Avatar className="h-5 w-5">
              <AvatarImage src={assignedUser?.user.image} />
              <AvatarFallback className="text-[9px]">{initials}</AvatarFallback>
            </Avatar>
          ) : (
            <UserRound className="h-4 w-4 text-muted-foreground" />
          )}
          {label}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-72 overflow-y-auto">
        {!isAssignedToMe && currentUser && (
          <DropdownMenuItem
            onClick={() => onAssign(currentUser.email || currentUser.username)}
          >
            Assign to me
          </DropdownMenuItem>
        )}
        {assignedTo && (
          <DropdownMenuItem onClick={() => onAssign('')}>
            Unassign
          </DropdownMenuItem>
        )}
        {users && users.length > 0 && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
              Assign to someone else
            </DropdownMenuLabel>
            {users.map((u) => (
              <DropdownMenuItem
                key={u.user.id}
                onClick={() => onAssign(u.user.email || u.user.username)}
              >
                {u.user.name}
              </DropdownMenuItem>
            ))}
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
