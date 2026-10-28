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
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, ShieldCheck } from 'lucide-react'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { ShieldSection } from './ShieldSection'

/** How many events the Shield tab shows; the rest are in the full list. */
export const PREVIEW_SIZE = 10

/**
 * The newest Shield events, on the environment's Shield tab. Every event,
 * with filters and paging, is in the full list (`ShieldEventsList`).
 */
export function ShieldEventsPanel({
  path,
  onShowAll,
}: {
  path: ShieldEnvironmentPath
  onShowAll: () => void
}) {
  const events = useQuery({
    ...listShieldEventsOptions({ path, query: { limit: PREVIEW_SIZE } }),
  })
  const rows = events.data?.events ?? []

  return (
    <ShieldSection
      id="shield-events"
      title="Recent events"
      description={`The ${PREVIEW_SIZE} newest requests with a verdict, and challenges served.`}
      actions={
        <Button type="button" variant="outline" size="sm" onClick={onShowAll}>
          All events
          <ArrowRight className="ml-2 h-4 w-4" />
        </Button>
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
          description="Requests Shield has a verdict for, and challenges it serves, appear here."
        />
      ) : (
        <EventsTable rows={rows} />
      )}
    </ShieldSection>
  )
}

export function EventsTable({ rows }: { rows: ShieldEventResponse[] }) {
  return (
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
      {/* IPv4 fits on one line; only an IPv6 address may wrap. */}
      <TableCell
        className={`font-mono text-xs ${event.ip.includes(':') ? 'break-all' : 'whitespace-nowrap'}`}
      >
        {event.ip}
      </TableCell>
      <TableCell className="hidden max-w-[18rem] md:table-cell">
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
      <TableCell className="hidden min-w-[14rem] lg:table-cell">
        <ul className="space-y-0.5">
          {event.reasons.map((reason) => (
            <li
              key={reason}
              className="font-mono text-xs [overflow-wrap:anywhere]"
            >
              {reason}
            </li>
          ))}
          {/* A signature match is usually a reason already ("CVE-… on /…"). */}
          {event.signature &&
            !event.reasons.some((reason) =>
              reason.startsWith(event.signature ?? '')
            ) && (
              <li className="font-mono text-xs [overflow-wrap:anywhere]">
                {event.signature}
              </li>
            )}
        </ul>
      </TableCell>
    </TableRow>
  )
}

export function EventsSkeleton() {
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
