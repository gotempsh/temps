// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listShieldEventsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ShieldEventResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { formatLocalDateTime } from '@/lib/date'
import {
  formatSolveMs,
  shieldEventKindLabel,
  shieldVerdictBadge,
} from '@/lib/shield'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { ChevronLeft, ChevronRight, ShieldCheck } from 'lucide-react'
import { useState } from 'react'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { ShieldSection } from './ShieldSection'

const PAGE_SIZE = 50

export function ShieldEventsPanel({ path }: { path: ShieldEnvironmentPath }) {
  // A stack of `before` cursors: the last one is the page shown, an empty
  // stack the newest events.
  const [cursors, setCursors] = useState<string[]>([])
  const before = cursors[cursors.length - 1]
  const events = useQuery({
    ...listShieldEventsOptions({
      path,
      query: { limit: PAGE_SIZE, before },
    }),
    placeholderData: keepPreviousData,
  })

  const rows = events.data?.events ?? []
  const nextBefore = events.data?.next_before ?? null

  return (
    <ShieldSection
      id="shield-events"
      title="Recent events"
      description="Requests with a verdict, and the challenges served, newest first."
      actions={
        <>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={cursors.length === 0 || events.isFetching}
            onClick={() => setCursors((stack) => stack.slice(0, -1))}
          >
            <ChevronLeft className="mr-1 h-4 w-4" />
            Newer
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={nextBefore === null || events.isFetching}
            onClick={() => {
              if (nextBefore) setCursors((stack) => [...stack, nextBefore])
            }}
          >
            Older
            <ChevronRight className="ml-1 h-4 w-4" />
          </Button>
        </>
      }
    >
      {events.error && (
        <div className="p-4">
          <ReadFailure
            resource="Shield events"
            error={events.error}
            cached={events.data !== undefined}
            onRetry={() => events.refetch()}
            retrying={events.isFetching}
            embedded
          />
        </div>
      )}
      {events.data === undefined ? (
        events.error ? null : (
          <EventsSkeleton />
        )
      ) : rows.length === 0 ? (
        <EmptyState
          size="compact"
          icon={ShieldCheck}
          title="No Shield events yet"
          description={
            cursors.length === 0
              ? 'Requests Shield has a verdict for, and challenges it serves, appear here.'
              : 'There are no older events.'
          }
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Time</TableHead>
              <TableHead>Event</TableHead>
              <TableHead>Client</TableHead>
              <TableHead className="hidden md:table-cell">Request</TableHead>
              <TableHead className="hidden lg:table-cell">Why</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((event, index) => (
              <EventRow
                key={`${event.request_id}-${event.kind}-${index}`}
                event={event}
              />
            ))}
          </TableBody>
        </Table>
      )}
    </ShieldSection>
  )
}

function EventRow({ event }: { event: ShieldEventResponse }) {
  return (
    <TableRow>
      <TableCell className="whitespace-nowrap text-sm text-muted-foreground">
        {formatLocalDateTime(event.timestamp, {
          dateStyle: 'short',
          timeStyle: 'medium',
        })}
      </TableCell>
      <TableCell>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant={shieldVerdictBadge(event.verdict)}>
            {event.verdict}
          </Badge>
          <span className="text-sm">{shieldEventKindLabel(event.kind)}</span>
        </div>
        <p className="mt-0.5 text-xs text-muted-foreground">
          {event.mode} mode
          {event.solve_ms != null &&
            ` · solved in ${formatSolveMs(event.solve_ms)}`}
          {event.difficulty != null && ` · difficulty ${event.difficulty}`}
        </p>
      </TableCell>
      <TableCell className="font-mono text-xs break-all">{event.ip}</TableCell>
      <TableCell className="hidden max-w-[22rem] md:table-cell">
        <p className="truncate text-sm" title={`${event.host}${event.path}`}>
          <span className="text-muted-foreground">{event.host}</span>
          {event.path}
        </p>
        {event.user_agent && (
          <p
            className="truncate text-xs text-muted-foreground"
            title={event.user_agent}
          >
            {event.user_agent}
          </p>
        )}
      </TableCell>
      <TableCell className="hidden lg:table-cell">
        <ul className="space-y-0.5">
          {event.reasons.map((reason) => (
            <li key={reason} className="font-mono text-xs break-all">
              {reason}
            </li>
          ))}
          {event.signature && (
            <li className="font-mono text-xs break-all">{event.signature}</li>
          )}
        </ul>
      </TableCell>
    </TableRow>
  )
}

function EventsSkeleton() {
  return (
    <div
      role="status"
      aria-label="Loading Shield events"
      className="space-y-2 p-4"
    >
      {[0, 1, 2, 3].map((item) => (
        <Skeleton key={item} className="h-10 w-full" />
      ))}
    </div>
  )
}
