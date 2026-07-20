'use client'

import { getEmailEvents, type TrackingEventResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ChevronLeft,
  ChevronRight,
  Globe,
  Monitor,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { EventBadge, EventIcon } from './shared'
import { parseUserAgent, problemMessage } from './sharedUtils'

// NOTE: the `/emails/{id}/tracking/events` endpoint (temps-email's
// `get_email_events` handler / `EventsQuery`) only supports an `event_type`
// filter — it has no `page`/`page_size` params and always returns the full
// event list for the email. Unlike EmailAnalytics.tsx's `fetchAllEvents`
// (which hits the paginated `/emails/events` endpoint), there is currently
// no server-side pagination available for a single email's event history.
// We fetch the full list once per (emailId, eventType) — bounded by the
// number of tracking events for one email — and paginate over it
// client-side, without keeping `page` in the query key so switching pages
// doesn't trigger a redundant network refetch of the same data.
async function fetchEmailEvents(
  emailId: string,
  eventType?: string
): Promise<TrackingEventResponse[]> {
  const response = await getEmailEvents({
    path: { id: emailId },
    query: eventType ? { event_type: eventType } : undefined,
  })
  if (response.error || !response.data) {
    throw new Error(problemMessage(response.error, 'Failed to fetch email events'))
  }
  return response.data
}

export function EmailEventTimeline({ emailId }: { emailId: string }) {
  const [eventType, setEventType] = useState<string | undefined>()
  const [page, setPage] = useState(1)
  const pageSize = 20

  const {
    data: events,
    isLoading,
    error,
  } = useQuery({
    queryKey: ['email-events', emailId, eventType],
    queryFn: () => fetchEmailEvents(emailId, eventType),
  })

  const total = events?.length ?? 0
  const totalPages = Math.ceil(total / pageSize)
  const data = useMemo(() => {
    if (!events) return undefined
    const start = (page - 1) * pageSize
    return {
      events: events.slice(start, start + pageSize),
      total,
    }
  }, [events, page, total])

  if (isLoading) {
    return (
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Activity</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {[1, 2, 3].map((i) => (
            <div key={i} className="flex items-start gap-3">
              <Skeleton className="h-8 w-8 rounded-full shrink-0" />
              <div className="space-y-1 flex-1">
                <Skeleton className="h-4 w-24" />
                <Skeleton className="h-3 w-48" />
              </div>
            </div>
          ))}
        </CardContent>
      </Card>
    )
  }

  if (error) {
    return (
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Activity</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            {error instanceof Error ? error.message : 'Failed to load events.'}
          </p>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <CardTitle className="text-base flex items-center gap-2">
            Activity
            {data && data.total > 0 && (
              <Badge variant="secondary" className="text-xs font-normal">
                {data.total}
              </Badge>
            )}
          </CardTitle>
          <Select
            value={eventType ?? 'all'}
            onValueChange={(v) => {
              setEventType(v === 'all' ? undefined : v)
              setPage(1)
            }}
          >
            <SelectTrigger className="w-[140px] h-8 text-xs">
              <SelectValue placeholder="All events" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All events</SelectItem>
              <SelectItem value="delivered">Delivered</SelectItem>
              {/* Stored event_type values are "open"/"click" — see
                  tracking_service.rs record_open/record_click. The endpoint
                  filters by exact match, so these values must match storage. */}
              <SelectItem value="open">Opened</SelectItem>
              <SelectItem value="click">Clicked</SelectItem>
              <SelectItem value="bounced">Bounced</SelectItem>
              <SelectItem value="complained">Complained</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </CardHeader>
      <CardContent>
        {!data || data.events.length === 0 ? (
          <p className="text-sm text-muted-foreground text-center py-4">
            No tracking events recorded yet.
          </p>
        ) : (
          <div className="space-y-0">
            {data.events.map((event, index) => (
              <div
                key={event.id}
                className="flex items-start gap-3 py-3 relative"
              >
                {/* Timeline line */}
                {index < data.events.length - 1 && (
                  <div className="absolute left-[15px] top-[40px] bottom-0 w-px bg-border" />
                )}

                {/* Icon circle */}
                <div className="flex items-center justify-center h-8 w-8 rounded-full bg-muted shrink-0 z-10">
                  <EventIcon type={event.event_type} />
                </div>

                {/* Content */}
                <div className="flex-1 min-w-0 space-y-1">
                  <div className="flex items-center gap-2 flex-wrap">
                    <EventBadge type={event.event_type} />
                    <span className="text-xs text-muted-foreground">
                      {format(new Date(event.created_at), 'PPp')}
                    </span>
                  </div>

                  <div className="flex items-center gap-3 text-xs text-muted-foreground flex-wrap">
                    {event.link_url && (
                      <span className="flex items-center gap-1 truncate max-w-[300px]">
                        <Globe className="h-3 w-3 shrink-0" />
                        <span className="truncate">{event.link_url}</span>
                      </span>
                    )}

                    {event.ip_address && (
                      <span className="flex items-center gap-1">
                        <Globe className="h-3 w-3 shrink-0" />
                        {event.ip_address}
                      </span>
                    )}

                    {event.user_agent && (
                      <span className="flex items-center gap-1">
                        <Monitor className="h-3 w-3 shrink-0" />
                        {parseUserAgent(event.user_agent)}
                      </span>
                    )}
                  </div>

                </div>
              </div>
            ))}
          </div>
        )}

        {/* Pagination */}
        {totalPages > 1 && (
          <div className="flex items-center justify-between pt-4 border-t mt-2">
            <p className="text-xs text-muted-foreground">
              {data!.total} events
            </p>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() => setPage((p) => p - 1)}
                disabled={page === 1}
              >
                <ChevronLeft className="h-3 w-3" />
              </Button>
              <span className="text-xs">
                {page} / {totalPages}
              </span>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() => setPage((p) => p + 1)}
                disabled={page >= totalPages}
              >
                <ChevronRight className="h-3 w-3" />
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
