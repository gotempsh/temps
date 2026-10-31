// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listShieldEventsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { SHIELD_EVENTS_PARAMS } from '@/lib/environment-navigation'
import { shieldEventKindLabel } from '@/lib/shield'
import { useUrlSearchText, useUrlState } from '@temps-sdk/ds'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  ChevronLeft,
  ChevronRight,
  Search,
  ShieldCheck,
} from 'lucide-react'
import { useState } from 'react'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { EventsSkeleton, EventsTable } from './ShieldEventsPanel'
import { ShieldSection } from './ShieldSection'

const PAGE_SIZE = 50

type UrlKey = (typeof SHIELD_EVENTS_PARAMS)[number]

const ANY = 'any'

const KINDS = [
  'verdict',
  'challenge_issued',
  'challenge_solved',
  'challenge_failed',
] as const

const VERDICTS = [
  { value: 'block', label: 'Block' },
  { value: 'challenge', label: 'Challenge' },
  { value: 'log', label: 'Log' },
] as const

/**
 * Every Shield event of one environment, newest first, filtered by kind,
 * verdict and client address. Route: the Shield tab with `?shield=events`.
 */
export function ShieldEventsList({
  path,
  onBack,
}: {
  path: ShieldEnvironmentPath
  onBack: () => void
}) {
  const url = useUrlState<UrlKey>()
  const [ipText, setIpText, ipCommitted] = useUrlSearchText('ip', {
    urlState: url,
  })
  const kind = KINDS.find((value) => value === url.state.kind)
  const verdict = VERDICTS.find(
    (option) => option.value === url.state.verdict
  )?.value
  const ip = ipCommitted.trim()

  // A stack of `before` cursors: the last one is the page shown, an empty
  // stack the newest events. A new filter starts again from the newest.
  const filterKey = `${kind ?? ''}|${verdict ?? ''}|${ip}`
  const [paging, setPaging] = useState({ filterKey, cursors: [] as string[] })
  const cursors = paging.filterKey === filterKey ? paging.cursors : []
  const setCursors = (next: (stack: string[]) => string[]) =>
    setPaging({ filterKey, cursors: next(cursors) })
  const before = cursors[cursors.length - 1]

  const events = useQuery({
    ...listShieldEventsOptions({
      path,
      query: {
        limit: PAGE_SIZE,
        ...(before ? { before } : {}),
        ...(kind ? { kind } : {}),
        ...(verdict ? { verdict } : {}),
        ...(ip ? { ip } : {}),
      },
    }),
    placeholderData: keepPreviousData,
    // A malformed address is refused by the server; retrying cannot help.
    retry: false,
  })
  const rows = events.data?.events ?? []
  const nextBefore = events.data?.next_before ?? null
  const isFiltered = kind !== undefined || verdict !== undefined || ip !== ''

  return (
    <div className="space-y-4">
      <Button type="button" variant="ghost" size="sm" onClick={onBack}>
        <ArrowLeft className="mr-2 h-4 w-4" />
        Shield overview
      </Button>
      <ShieldSection
        id="shield-events"
        title="All events"
        level={2}
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
        <div className="flex flex-col gap-2 border-b p-4 sm:flex-row sm:flex-wrap sm:items-center">
          <div className="relative w-full sm:w-64">
            <Search
              aria-hidden
              className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              type="search"
              value={ipText}
              onChange={(event) => setIpText(event.target.value)}
              placeholder="Client address"
              aria-label="Client address"
              className="pl-8 font-mono"
              maxLength={45}
            />
          </div>
          <Select
            value={kind ?? ANY}
            onValueChange={(value) =>
              url.patch({ kind: value === ANY ? undefined : value })
            }
          >
            <SelectTrigger className="w-full sm:w-44" aria-label="Event kind">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY}>Any event</SelectItem>
              {KINDS.map((value) => (
                <SelectItem key={value} value={value}>
                  {shieldEventKindLabel(value)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select
            value={verdict ?? ANY}
            onValueChange={(value) =>
              url.patch({ verdict: value === ANY ? undefined : value })
            }
          >
            <SelectTrigger className="w-full sm:w-40" aria-label="Verdict">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ANY}>Any verdict</SelectItem>
              {VERDICTS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {isFiltered && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => {
                setIpText('')
                url.patch({
                  kind: undefined,
                  verdict: undefined,
                  ip: undefined,
                })
              }}
            >
              Clear filters
            </Button>
          )}
        </div>
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
            title={isFiltered ? 'No events match' : 'No Shield events yet'}
            description={
              cursors.length > 0
                ? 'There are no older events.'
                : isFiltered
                  ? 'No event matches these filters.'
                  : 'Requests Shield has a verdict for, and challenges it serves, appear here.'
            }
          />
        ) : (
          <EventsTable rows={rows} />
        )}
      </ShieldSection>
    </div>
  )
}
