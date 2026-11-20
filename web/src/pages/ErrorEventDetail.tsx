// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { PageTitle } from '@temps-sdk/ds'
import { HighlightedCode } from '@/components/ui/code-block'

import { ProjectResponse } from '@/api/client'
import { getErrorEventOptions } from '@/api/client/@tanstack/react-query.gen'
import { SentryEventDetail } from '@/components/error-tracking/SentryEventDetail'
import { CspReportDetails } from '@/components/error-tracking/CspReportDetails'
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
import { extractSentryEvent, sentryExceptions } from '@/lib/sentry-utils'
import { readCspReport } from '@/lib/csp-report'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  Clock,
  RotateCcw,
} from 'lucide-react'
import { useEffect } from 'react'
import { useNavigate, useParams } from 'react-router'

export function ErrorEventDetail({ project }: { project: ProjectResponse }) {
  const { projectSlug, errorGroupId, eventId } = useParams<{
    projectSlug: string
    errorGroupId: string
    eventId: string
  }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // Fetch event details
  const {
    data: event,
    isLoading,
    error: eventError,
    refetch: refetchEvent,
  } = useQuery({
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
  const isCsp = Boolean(readCspReport(event?.data))
  const primaryException = sentryEvent
    ? sentryExceptions(sentryEvent).slice(-1)[0]
    : undefined

  usePageTitle(
    sentryEvent
      ? `Event: ${primaryException?.type || 'Event'}`
      : 'Event Details'
  )

  useEffect(() => {
    if (event && projectSlug) {
      setBreadcrumbs([
        { label: 'Projects', href: '/projects' },
        { label: projectSlug, href: `/projects/${projectSlug}` },
        { label: 'Error tracking', href: `/projects/${projectSlug}/errors` },
        {
          label: 'Error group',
          href: `/projects/${projectSlug}/errors/${errorGroupId}`,
        },
        { label: 'Event details' },
      ])
    }
  }, [setBreadcrumbs, event, projectSlug, errorGroupId])

  const getSeverityColor = (level: string) => {
    switch (level?.toLowerCase()) {
      case 'error':
      case 'fatal':
        return 'text-red-700 dark:text-red-600 bg-red-100 dark:bg-red-900/20'
      case 'warning':
        return 'text-yellow-700 dark:text-yellow-600 bg-yellow-100 dark:bg-yellow-900/20'
      case 'info':
        return 'text-blue-600 bg-blue-100 dark:bg-blue-900/20'
      default:
        return 'text-gray-600 bg-gray-100 dark:bg-gray-900/20'
    }
  }

  if (isLoading) {
    return (
      <div className="space-y-6">
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

  const isNotFound =
    (eventError as any)?.status === 404 ||
    (eventError as any)?.title === 'Not Found'

  if (!event && eventError && !isNotFound) {
    return (
      <div className="flex flex-col items-center justify-center gap-4 min-h-[400px]">
        <AlertCircle className="h-8 w-8 text-destructive" />
        <div className="text-center">
          <h2 className="text-lg font-semibold">Failed to load error event</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {eventError instanceof Error
              ? eventError.message
              : 'An unexpected error occurred. Please try again.'}
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={() => void refetchEvent()}>
            <RotateCcw className="mr-2 h-4 w-4" />
            Retry
          </Button>
          <Button
            variant="ghost"
            onClick={() =>
              navigate(`/projects/${project.slug}/errors/${errorGroupId}`)
            }
          >
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to error group
          </Button>
        </div>
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
    <div>
      {/* Event Title with Back Button */}
      <div className="mb-6">
        <div className="flex items-start gap-3 mb-2">
          <Button
            aria-label="Back"
            variant="ghost"
            size="sm"
            onClick={() =>
              navigate(`/projects/${project.slug}/errors/${errorGroupId}`)
            }
            className="mt-1"
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2 mb-2">
              <Badge
                className={cn(
                  getSeverityColor(sentryEvent?.sentry?.level || 'error')
                )}
              >
                {sentryEvent?.sentry?.level || 'error'}
              </Badge>
              <PageTitle className="min-w-0 break-words text-lg font-semibold sm:text-2xl">
                {primaryException?.value ||
                  sentryEvent?.sentry?.logentry?.formatted ||
                  (isCsp
                    ? 'Content Security Policy violation'
                    : 'Event Details')}
              </PageTitle>
            </div>
            <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground">
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

      <CspReportDetails data={event.data} />
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
            <CardTitle>Event data</CardTitle>
            <CardDescription>
              {format(new Date(event.timestamp), 'PPpp')}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <ScrollArea className="h-[600px]">
              <pre className="text-xs">
                <HighlightedCode
                  code={JSON.stringify(event, null, 2)}
                  language="json"
                />
              </pre>
            </ScrollArea>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
