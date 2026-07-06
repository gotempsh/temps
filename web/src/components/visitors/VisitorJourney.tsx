import { getVisitorJourneyOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  JourneyEvent,
  JourneySession,
  ProjectResponse,
} from '@/api/client/types.gen'
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
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useQuery } from '@tanstack/react-query'
import { format, formatDistanceToNow } from 'date-fns'
import {
  ChevronDown,
  ChevronRight,
  Clock,
  ExternalLink,
  Eye,
  FileText,
  Globe,
  LogIn,
  LogOut,
  MousePointer,
  Zap,
} from 'lucide-react'
import * as React from 'react'

interface VisitorJourneyProps {
  project: ProjectResponse
  visitorId: number
}

// --- Helper functions ---

function formatDuration(seconds: number): string {
  if (seconds < 1) return '<1s'
  if (seconds < 60) return `${Math.round(seconds)}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${Math.round(seconds % 60)}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

function channelLabel(channel: string | null | undefined): string {
  if (!channel) return 'Direct'
  const map: Record<string, string> = {
    direct: 'Direct',
    organic: 'Organic Search',
    social: 'Social',
    referral: 'Referral',
    paid: 'Paid',
    email: 'Email',
    display: 'Display',
    affiliate: 'Affiliate',
  }
  return map[channel.toLowerCase()] || channel
}

function channelColor(
  channel: string | null | undefined
): string {
  if (!channel) return 'bg-muted text-muted-foreground'
  const map: Record<string, string> = {
    direct: 'bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300',
    organic:
      'bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300',
    social:
      'bg-purple-100 text-purple-800 dark:bg-purple-900/30 dark:text-purple-300',
    referral:
      'bg-orange-100 text-orange-800 dark:bg-orange-900/30 dark:text-orange-300',
    paid: 'bg-red-100 text-red-800 dark:bg-red-900/30 dark:text-red-300',
    email:
      'bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300',
  }
  return (
    map[(channel || '').toLowerCase()] ||
    'bg-muted text-muted-foreground'
  )
}

// --- Sub-components ---

function SessionStartNode({ session }: { session: JourneySession }) {
  const hasUtm =
    session.utm_source || session.utm_medium || session.utm_campaign
  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-400">
        <LogIn className="h-3.5 w-3.5" />
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-medium">Session started</span>
          <Badge
            variant="outline"
            className={`text-xs ${channelColor(session.channel)}`}
          >
            {channelLabel(session.channel)}
          </Badge>
          {session.is_bounced && (
            <Badge
              variant="outline"
              className="text-xs bg-red-50 text-red-700 dark:bg-red-900/20 dark:text-red-400"
            >
              Bounce
            </Badge>
          )}
          {session.is_engaged && (
            <Badge
              variant="outline"
              className="text-xs bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
            >
              Engaged
            </Badge>
          )}
        </div>
        {session.referrer_hostname && (
          <p className="text-xs text-muted-foreground flex items-center gap-1">
            <Globe className="h-3 w-3" />
            from {session.referrer_hostname}
          </p>
        )}
        {hasUtm && (
          <div className="flex flex-wrap gap-1 mt-1">
            {session.utm_source && (
              <Badge variant="secondary" className="text-xs">
                source: {session.utm_source}
              </Badge>
            )}
            {session.utm_medium && (
              <Badge variant="secondary" className="text-xs">
                medium: {session.utm_medium}
              </Badge>
            )}
            {session.utm_campaign && (
              <Badge variant="secondary" className="text-xs">
                campaign: {session.utm_campaign}
              </Badge>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

function SessionEndNode({ session }: { session: JourneySession }) {
  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400">
        <LogOut className="h-3.5 w-3.5" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-medium text-muted-foreground">
            Session ended
          </span>
          <span className="text-xs text-muted-foreground">
            {formatDuration(session.duration_seconds)}
          </span>
        </div>
        {session.exit_path && (
          <p className="text-xs text-muted-foreground mt-0.5">
            Exit page: {session.exit_path}
          </p>
        )}
      </div>
    </div>
  )
}

function PageViewNode({ event }: { event: JourneyEvent }) {
  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-400">
        <Eye className="h-3.5 w-3.5" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-mono truncate max-w-[300px]">
            {event.page_path || '/'}
          </span>
          {event.is_entry && (
            <Badge
              variant="outline"
              className="text-xs bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400"
            >
              Entry
            </Badge>
          )}
          {event.is_exit && (
            <Badge
              variant="outline"
              className="text-xs bg-gray-50 text-gray-700 dark:bg-gray-900/20 dark:text-gray-400"
            >
              Exit
            </Badge>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-3 mt-0.5 text-xs text-muted-foreground">
          {event.time_on_page != null && event.time_on_page > 0 && (
            <span className="flex items-center gap-1">
              <Clock className="h-3 w-3" />
              {formatDuration(event.time_on_page)}
            </span>
          )}
          {event.scroll_depth != null && event.scroll_depth > 0 && (
            <span className="flex items-center gap-1">
              <MousePointer className="h-3 w-3" />
              {event.scroll_depth}% scrolled
            </span>
          )}
          {event.page_title && (
            <span className="flex items-center gap-1 truncate max-w-[200px]">
              <FileText className="h-3 w-3" />
              {event.page_title}
            </span>
          )}
          {event.referrer && (
            <span className="flex items-center gap-1 truncate max-w-[200px]">
              <ExternalLink className="h-3 w-3" />
              {event.referrer}
            </span>
          )}
        </div>
      </div>
      <span className="shrink-0 text-xs text-muted-foreground">
        {format(new Date(event.occurred_at), 'HH:mm:ss')}
      </span>
    </div>
  )
}

function CustomEventNode({ event }: { event: JourneyEvent }) {
  const [expanded, setExpanded] = React.useState(false)
  const hasData = Boolean(
    event.event_data && Object.keys(event.event_data).length > 0
  )

  return (
    <div className="flex items-start gap-3">
      <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-400">
        <Zap className="h-3.5 w-3.5" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <Badge className="text-xs bg-amber-100 text-amber-800 dark:bg-amber-900/30 dark:text-amber-300 border-amber-200 dark:border-amber-800">
            {event.event_name}
          </Badge>
          {event.page_path && (
            <span className="text-xs text-muted-foreground font-mono">
              on {event.page_path}
            </span>
          )}
          {hasData && (
            <Button
              variant="ghost"
              size="sm"
              className="h-5 px-1.5 text-xs"
              onClick={() => setExpanded(!expanded)}
            >
              {expanded ? 'Hide' : 'Data'}
              <ChevronDown
                className={`h-3 w-3 ml-0.5 transition-transform ${expanded ? 'rotate-180' : ''}`}
              />
            </Button>
          )}
        </div>
        {expanded && hasData && (
          <pre className="mt-1.5 rounded bg-muted p-2 text-xs font-mono overflow-x-auto max-h-48">
            {JSON.stringify(event.event_data, null, 2)}
          </pre>
        )}
      </div>
      <span className="shrink-0 text-xs text-muted-foreground">
        {format(new Date(event.occurred_at), 'HH:mm:ss')}
      </span>
    </div>
  )
}

function EventNode({ event }: { event: JourneyEvent }) {
  if (event.event_type === 'page_view') {
    return <PageViewNode event={event} />
  }
  return <CustomEventNode event={event} />
}

function SessionGroup({ session }: { session: JourneySession }) {
  const [open, setOpen] = React.useState(true)
  const sessionDate = new Date(session.started_at)

  return (
    <Collapsible open={open} onOpenChange={setOpen} className="group">
      <CollapsibleTrigger asChild>
        <button type="button" className="flex w-full items-center gap-3 rounded-lg border bg-card px-4 py-3 text-left hover:bg-accent/50 transition-colors">
          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary">
            {open ? (
              <ChevronDown className="h-4 w-4" />
            ) : (
              <ChevronRight className="h-4 w-4" />
            )}
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="text-sm font-medium">
                      {format(sessionDate, 'MMM d, yyyy')}
                    </span>
                  </TooltipTrigger>
                  <TooltipContent>
                    {format(sessionDate, 'PPpp')}
                  </TooltipContent>
                </Tooltip>
              </TooltipProvider>
              <span className="text-xs text-muted-foreground">
                {formatDistanceToNow(sessionDate, { addSuffix: true })}
              </span>
            </div>
            <div className="flex flex-wrap items-center gap-3 mt-0.5 text-xs text-muted-foreground">
              <span>{session.page_views} page{session.page_views !== 1 ? 's' : ''}</span>
              <span>{formatDuration(session.duration_seconds)}</span>
              {session.entry_path && (
                <span className="font-mono truncate max-w-[180px]">
                  {session.entry_path}
                </span>
              )}
            </div>
          </div>
          <Badge
            variant="outline"
            className={`shrink-0 text-xs ${channelColor(session.channel)}`}
          >
            {channelLabel(session.channel)}
          </Badge>
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="ml-[1.375rem] border-l-2 border-border pl-6 pt-3 pb-1 space-y-3">
          {/* Session start */}
          <SessionStartNode session={session} />

          {/* Events */}
          {session.events.map((event) => (
            <EventNode key={event.id} event={event} />
          ))}

          {/* Session end */}
          <SessionEndNode session={session} />
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}

// --- Loading skeleton ---

function JourneySkeleton() {
  return (
    <div className="space-y-4">
      {['skel-1', 'skel-2', 'skel-3'].map((id) => (
        <div key={id} className="space-y-3">
          <div className="flex items-center gap-3 rounded-lg border px-4 py-3">
            <Skeleton className="h-8 w-8 rounded-full" />
            <div className="flex-1 space-y-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-60" />
            </div>
            <Skeleton className="h-5 w-16 rounded-full" />
          </div>
          <div className="ml-[1.375rem] border-l-2 border-border pl-6 space-y-3">
            {[`${id}-a`, `${id}-b`, `${id}-c`].map((eventId) => (
              <div key={eventId} className="flex items-start gap-3">
                <Skeleton className="h-7 w-7 rounded-full shrink-0" />
                <div className="flex-1 space-y-1.5">
                  <Skeleton className="h-4 w-48" />
                  <Skeleton className="h-3 w-32" />
                </div>
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  )
}

// --- Main component ---

export function VisitorJourney({ project, visitorId }: VisitorJourneyProps) {
  const {
    data: journey,
    isLoading,
    error,
  } = useQuery({
    ...getVisitorJourneyOptions({
      path: { visitor_id: visitorId },
      query: { project_id: project.id },
    }),
  })

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Visitor Journey</CardTitle>
          <CardDescription>Loading journey data...</CardDescription>
        </CardHeader>
        <CardContent>
          <JourneySkeleton />
        </CardContent>
      </Card>
    )
  }

  if (error) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Visitor Journey</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="flex flex-col items-center justify-center py-12">
            <p className="text-muted-foreground mb-2">
              Failed to load journey data
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        </CardContent>
      </Card>
    )
  }

  if (!journey || journey.sessions.length === 0) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Visitor Journey</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="flex flex-col items-center justify-center py-12">
            <Globe className="h-12 w-12 text-muted-foreground mb-4" />
            <p className="text-muted-foreground mb-2">No journey data yet</p>
            <p className="text-sm text-muted-foreground">
              Events will appear here as the visitor navigates your site
            </p>
          </div>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Visitor Journey</CardTitle>
            <CardDescription>
              {journey.total_sessions} session
              {journey.total_sessions !== 1 ? 's' : ''},{' '}
              {journey.total_events} event
              {journey.total_events !== 1 ? 's' : ''}
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        <div className="space-y-4">
          {journey.sessions.map((session) => (
            <SessionGroup key={session.session_id} session={session} />
          ))}
        </div>
      </CardContent>
    </Card>
  )
}
