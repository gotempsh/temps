// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { getGlobalEvents, type PaginatedEventsResponse } from '@/api/client'
import { IpLink } from '@/components/data-display/IpLink'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
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
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { RecordLink, RecordRow } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { AlertCircle, Globe, Monitor } from 'lucide-react'
import { useState } from 'react'
import { EventBadge } from './shared'
import { parseUserAgent, problemMessage } from './sharedUtils'

const PAGE_SIZE = 10

async function fetchEvents(params: {
  event_type?: string
  page: number
  page_size: number
}): Promise<PaginatedEventsResponse> {
  const response = await getGlobalEvents({ query: params })
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to fetch email events')
    )
  }
  return response.data
}

/**
 * Opens, clicks, bounces and complaints across every email. The feed includes
 * recipient IPs and clicked URLs, so the server only serves it to instance
 * administrators; anyone else is told so rather than handed a refused request.
 */
export function EmailEventLog() {
  const isAdmin = useIsInstanceAdmin()
  const [eventType, setEventType] = useState<string | undefined>()
  const [page, setPage] = useState(1)

  const eventsQuery = useQuery({
    queryKey: ['all-email-events', eventType, page],
    queryFn: () =>
      fetchEvents({ event_type: eventType, page, page_size: PAGE_SIZE }),
    enabled: isAdmin,
  })
  const events = eventsQuery.data
  const totalPages = events ? Math.ceil(events.total / PAGE_SIZE) : 0

  if (!isAdmin) {
    return (
      <div className="space-y-2">
        <h2 className="text-base font-semibold">Event log</h2>
        <p className="text-sm text-muted-foreground">
          Only an instance administrator can see events across every email. Open
          an email to see its own.
        </p>
      </div>
    )
  }

  return (
    <div className="@container space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h2 className="text-base font-semibold">Event log</h2>
        <Select
          value={eventType ?? 'all'}
          onValueChange={(v) => {
            setEventType(v === 'all' ? undefined : v)
            setPage(1)
          }}
        >
          <SelectTrigger
            aria-label="Filter by event"
            className="w-full sm:w-[160px]"
          >
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

      {eventsQuery.isLoading ? (
        <div className="space-y-2">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      ) : eventsQuery.isError ? (
        <Alert role="alert">
          <AlertCircle className="size-4" />
          <AlertTitle>Couldn&apos;t load the event log</AlertTitle>
          <AlertDescription className="space-y-3">
            <p>{eventsQuery.error.message}</p>
            <Button
              size="sm"
              variant="outline"
              disabled={eventsQuery.isFetching}
              onClick={() => void eventsQuery.refetch()}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      ) : !events || events.events.length === 0 ? (
        <p className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
          {eventType
            ? 'No events match this filter.'
            : 'No events yet. They appear once emails sent with tracking are opened, clicked or bounce.'}
        </p>
      ) : (
        <>
          <div className="rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Event</TableHead>
                  <TableHead className="hidden @2xl:table-cell">Link</TableHead>
                  <TableHead className="hidden @4xl:table-cell">
                    Source
                  </TableHead>
                  <TableHead>Time</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.events.map((event) => (
                  <RecordRow key={event.id}>
                    <TableCell>
                      <RecordLink
                        to={`/email/${event.email_id}`}
                        className="min-h-0"
                      >
                        <EventBadge
                          type={event.event_type}
                          iconClassName="h-3.5 w-3.5"
                        />
                      </RecordLink>
                    </TableCell>
                    <TableCell className="hidden max-w-[320px] @2xl:table-cell">
                      {event.link_url ? (
                        <span className="flex items-center gap-1 text-xs text-muted-foreground">
                          <Globe className="h-3 w-3 shrink-0" />
                          <span className="truncate">{event.link_url}</span>
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          --
                        </span>
                      )}
                    </TableCell>
                    <TableCell className="hidden @4xl:table-cell">
                      <div className="flex items-center gap-2 text-xs text-muted-foreground">
                        {event.ip_address && (
                          <span className="flex items-center gap-1">
                            <Globe className="h-3 w-3" />
                            <IpLink ip={event.ip_address} />
                          </span>
                        )}
                        {event.user_agent && (
                          <span className="flex items-center gap-1">
                            <Monitor className="h-3 w-3" />
                            {parseUserAgent(event.user_agent)}
                          </span>
                        )}
                        {!event.ip_address && !event.user_agent && '--'}
                      </div>
                    </TableCell>
                    <TableCell className="text-muted-foreground text-sm whitespace-nowrap">
                      {formatDistanceToNow(new Date(event.created_at), {
                        addSuffix: true,
                      })}
                    </TableCell>
                  </RecordRow>
                ))}
              </TableBody>
            </Table>
          </div>

          {totalPages > 1 && (
            <ResponsivePagination
              page={page}
              pageSize={PAGE_SIZE}
              total={events.total}
              totalPages={totalPages}
              onPageChange={setPage}
              ariaLabel="Event log pagination"
            />
          )}
        </>
      )}
    </div>
  )
}
