import { ErrorEventResponse, ProjectResponse } from '@/api/client'
import {
  getErrorGroupOptions,
  listErrorEventsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AutofixButton } from '@/components/autofixer/AutofixButton'
import { SentryEventDetail } from '@/components/error-tracking/SentryEventDetail'
import { SentryListItem } from '@/components/error-tracking/SentryListItem'
import {
  updateErrorGroupMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useAssistantPageContext } from '@/components/ai/AiAssistantContext'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import { extractSentryEvent } from '@/lib/sentry-utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import { AlertTriangle, ArrowLeft, Check, EyeOff, RotateCcw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'

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
  const { data: errorGroup, isLoading: isLoadingGroup } = useQuery({
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
      <div className="mb-6">
        <div className="flex items-start gap-3 mb-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => navigate(`/projects/${project.slug}/errors`)}
            className="mt-1"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div className="flex-1">
            <div className="flex flex-wrap items-center gap-3 mb-2">
              <Badge
                className={cn(
                  getSeverityColor(errorGroup.error_type || 'error')
                )}
              >
                {errorGroup.error_type || 'error'}
              </Badge>
              <h1 className="text-xl sm:text-2xl font-semibold flex-1 min-w-0 break-words">{errorGroup.title}</h1>
              <div className="flex flex-wrap gap-2 flex-shrink-0">
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
                {(errorGroup as any).status !== 'ignored' && (errorGroup as any).status !== 'resolved' && (
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
                {((errorGroup as any).status === 'resolved' || (errorGroup as any).status === 'ignored') && (
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
            </div>
            <div className="flex flex-wrap items-center gap-x-6 gap-y-2 text-sm text-muted-foreground">
              {(errorGroup as any).status && (errorGroup as any).status !== 'unresolved' && (
                <Badge variant={(errorGroup as any).status === 'resolved' ? 'default' : 'secondary'} className="text-xs">
                  {(errorGroup as any).status}
                </Badge>
              )}
              <span>{errorGroup.total_count || 0} occurrences</span>
              <span>•</span>
              <span>
                First seen <TimeAgo date={errorGroup.first_seen} />
              </span>
              <span>•</span>
              <span>
                Last seen <TimeAgo date={errorGroup.last_seen} />
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Autofix section */}
      {project.git_provider_connection_id && (
        <div className="mb-6">
          <AutofixButton
            projectId={project.id}
            projectSlug={project.slug}
            errorGroupId={parseInt(errorGroupId!)}
          />
        </div>
      )}

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
                      const d = latestEvent.data as Record<string, unknown> | undefined
                      if (d && typeof d === 'object') {
                        for (const key of ['message', 'request', 'user', 'tags', 'contexts', 'environment', 'release']) {
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
