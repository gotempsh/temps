import { ProjectResponse } from '@/api/client'
import { getErrorEventOptions } from '@/api/client/@tanstack/react-query.gen'
import { SentryEventDetail } from '@/components/error-tracking/SentryEventDetail'
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
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn } from '@/lib/utils'
import { extractSentryEvent } from '@/lib/sentry-utils'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { AlertTriangle, ArrowLeft, Clock } from 'lucide-react'
import { useEffect } from 'react'
import { useNavigate, useParams } from 'react-router-dom'

export function ErrorEventDetail({ project }: { project: ProjectResponse }) {
  const { projectSlug, errorGroupId, eventId } = useParams<{
    projectSlug: string
    errorGroupId: string
    eventId: string
  }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // Fetch event details
  const { data: event, isLoading } = useQuery({
    ...getErrorEventOptions({
      path: {
        event_id: parseInt(eventId!),
        group_id: parseInt(errorGroupId!),
        project_id: project.id,
      },
    }),
    enabled: !!eventId && !!errorGroupId,
  })

  const sentryEvent = event ? extractSentryEvent(event.data) : null

  usePageTitle(
    sentryEvent
      ? `Event: ${sentryEvent.sentry.exception?.values?.[0]?.type || 'Event'}`
      : 'Event Details'
  )

  useEffect(() => {
    if (event && projectSlug) {
      setBreadcrumbs([
        { label: 'Projects', href: '/projects' },
        { label: projectSlug, href: `/projects/${projectSlug}` },
        { label: 'Error Tracking', href: `/projects/${projectSlug}/errors` },
        {
          label: 'Error Group',
          href: `/projects/${projectSlug}/errors/${errorGroupId}`,
        },
        { label: 'Event Details' },
      ])
    }
  }, [setBreadcrumbs, event, projectSlug, errorGroupId])

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

  if (isLoading) {
    return (
      <div className="space-y-6 p-6">
        <Skeleton className="h-8 w-64" />
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

  if (!event) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>Event not found</AlertDescription>
        </Alert>
      </div>
    )
  }

  return (
    <div className="p-6">
      {/* Event Title with Back Button */}
      <div className="mb-6">
        <div className="flex items-start gap-3 mb-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              navigate(`/projects/${project.slug}/errors/${errorGroupId}`)
            }
            className="mt-1"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div className="flex-1">
            <div className="flex items-center gap-3 mb-2">
              <Badge
                className={cn(
                  getSeverityColor(sentryEvent?.sentry?.level || 'error')
                )}
              >
                {sentryEvent?.sentry?.level || 'error'}
              </Badge>
              <h1 className="text-2xl font-semibold">
                {sentryEvent?.sentry?.exception?.values?.[0]?.value ||
                  sentryEvent?.sentry?.logentry?.formatted ||
                  'Event Details'}
              </h1>
            </div>
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Clock className="h-3 w-3" />
              <span>{format(new Date(event.timestamp), 'PPpp')}</span>
              {sentryEvent && (
                <>
                  <span>•</span>
                  <span className="font-mono text-xs">
                    Event ID: {sentryEvent.sentry.event_id}
                  </span>
                </>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Event Content */}
      {sentryEvent ? (
        <SentryEventDetail
          event={sentryEvent}
          showRawData={true}
          showHeader={false}
        />
      ) : (
        <Card>
          <CardHeader>
            <CardTitle>Event Data</CardTitle>
            <CardDescription>
              {format(new Date(event.timestamp), 'PPpp')}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <ScrollArea className="h-[600px]">
              <pre className="text-xs">{JSON.stringify(event, null, 2)}</pre>
            </ScrollArea>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
