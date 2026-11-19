// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Overview cards for the events the tracker records by itself (ADR 071 D1):
 * outbound link clicks, file downloads and pages visitors could not find.
 *
 * Each card renders whether or not there is data. An empty card says what it
 * will show and how to get there, rather than disappearing.
 */

import { aggregateApiTraffic } from '@/api/client'
import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { client } from '@/api/client/client.gen'
import type {
  ProjectResponse,
  TrafficAggregationResponse,
} from '@/api/client/types.gen'
import { RankedList } from '@/components/data-display/RankedList'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  FILE_DOWNLOAD_EVENT,
  NOT_FOUND_EVENT,
  OUTBOUND_CLICK_EVENT,
  compactUrl,
  mergeNotFoundRows,
  notFoundPathsRequest,
  notFoundReferrersRequest,
  notFoundWindow,
  propertyValueLabel,
  type EventPropertiesResponse,
} from '@/lib/auto-events'
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { ExternalLink } from 'lucide-react'
import type { ReactNode } from 'react'
import { Link } from 'react-router'

const DOCS_URL = 'https://temps.sh/docs/analytics#automatic-events'

interface AutoEventsCardProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

/**
 * `GET /analytics/event-properties` is called through the shared client
 * directly until the generated client is refreshed from a server that has it.
 */
async function fetchEventProperties(params: {
  projectId: number
  eventName: string
  property: string
  startDate: Date
  endDate: Date
  environment: number | undefined
}): Promise<EventPropertiesResponse> {
  const { data, error } = await client.get<
    { 200: EventPropertiesResponse },
    unknown,
    false
  >({
    security: [{ scheme: 'bearer', type: 'http' }],
    url: '/analytics/event-properties',
    query: {
      project_id: params.projectId,
      event_name: params.eventName,
      property: params.property,
      start_date: params.startDate.toISOString(),
      end_date: params.endDate.toISOString(),
      environment_id: params.environment,
      limit: 10,
    },
  })
  if (error || !data) {
    throw new Error(
      `Could not load ${params.eventName} by ${params.property} for project ${params.projectId}`
    )
  }
  return data
}

function CardSkeleton() {
  return (
    <div className="space-y-3 py-2">
      {['a', 'b', 'c', 'd'].map((key) => (
        <div key={key} className="flex items-center justify-between gap-4">
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-12" />
        </div>
      ))}
    </div>
  )
}

/** A compact empty state that says what the card shows and how to fill it. */
function EmptyExplainer({
  children,
  setupHref,
}: {
  children: ReactNode
  setupHref?: string
}) {
  return (
    <div className="space-y-2 py-4 text-sm text-muted-foreground">
      {children}
      <div className="flex flex-wrap gap-x-4 gap-y-1">
        {setupHref && (
          <Link className="text-primary hover:underline" to={setupHref}>
            Tracker setup
          </Link>
        )}
        <a
          className="inline-flex items-center gap-1 text-primary hover:underline"
          href={DOCS_URL}
          target="_blank"
          rel="noreferrer"
        >
          How it works
          <ExternalLink className="h-3 w-3" aria-hidden="true" />
        </a>
      </div>
    </div>
  )
}

function LoadError({ what }: { what: string }) {
  return (
    <p className="py-4 text-sm text-muted-foreground">
      Could not load {what}. Refresh the page to try again.
    </p>
  )
}

function eventDetailHref(project: ProjectResponse, eventName: string): string {
  return `/projects/${project.slug}/analytics/events/${encodeURIComponent(eventName)}`
}

/**
 * Ranks one auto-tracked event by one of its properties: the destinations of
 * outbound clicks, or the files downloaded.
 */
function EventPropertyCard({
  project,
  startDate,
  endDate,
  environment,
  eventName,
  property,
  title,
  description,
  emptyText,
  formatValue,
}: AutoEventsCardProps & {
  eventName: string
  property: string
  title: string
  description: string
  emptyText: ReactNode
  formatValue: (value: string | null) => string
}) {
  const { data, isLoading, isError } = useQuery({
    queryKey: [
      'event-properties',
      project.id,
      eventName,
      property,
      startDate?.toISOString(),
      endDate?.toISOString(),
      environment,
    ],
    queryFn: () =>
      fetchEventProperties({
        projectId: project.id,
        eventName,
        property,
        startDate: startDate as Date,
        endDate: endDate as Date,
        environment,
      }),
    enabled: !!startDate && !!endDate,
  })

  const items = data?.items ?? []

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between gap-2">
          <div>
            <CardTitle>{title}</CardTitle>
            <CardDescription>{description}</CardDescription>
          </div>
          {items.length > 0 && (
            <Button variant="ghost" size="sm" className="text-xs" asChild>
              <Link to={eventDetailHref(project, eventName)}>Each event</Link>
            </Button>
          )}
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <CardSkeleton />
        ) : isError ? (
          <LoadError what={title.toLowerCase()} />
        ) : items.length === 0 ? (
          <EmptyExplainer
            setupHref={`/projects/${project.slug}/analytics/setup`}
          >
            {emptyText}
          </EmptyExplainer>
        ) : (
          <RankedList
            label={title}
            items={items.map((item) => ({
              id: item.value ?? '(not set)',
              title: (
                <span title={item.value ?? undefined}>
                  {formatValue(item.value)}
                </span>
              ),
              facts: [
                { label: 'Clicks', value: item.count.toLocaleString() },
                {
                  label: 'Visitors',
                  value: item.unique_visitors.toLocaleString(),
                },
                { label: 'Share', value: `${item.percentage.toFixed(1)}%` },
              ],
            }))}
          />
        )}
      </CardContent>
      {data && items.length > 0 && (
        <CardFooter className="text-sm text-muted-foreground">
          {data.total_count.toLocaleString()} in this period
        </CardFooter>
      )}
    </Card>
  )
}

export function OutboundLinksCard(props: AutoEventsCardProps) {
  return (
    <EventPropertyCard
      {...props}
      eventName={OUTBOUND_CLICK_EVENT}
      property="url"
      title="Outbound links"
      description="Links to other sites that visitors clicked"
      formatValue={(value) =>
        value ? compactUrl(value) : propertyValueLabel(value)
      }
      emptyText={
        <p>
          No outbound link clicks in this period. The Temps tracker records a
          click on any link to another site as <code>outbound_click</code>, with
          no code on your side.
        </p>
      }
    />
  )
}

export function FileDownloadsCard(props: AutoEventsCardProps) {
  return (
    <EventPropertyCard
      {...props}
      eventName={FILE_DOWNLOAD_EVENT}
      property="url"
      title="File downloads"
      description="Documents, archives, media and installers visitors downloaded"
      formatValue={(value) =>
        value ? compactUrl(value) : propertyValueLabel(value)
      }
      emptyText={
        <p>
          No file downloads in this period. The tracker records a click on a
          link to a PDF, ZIP, DMG and other common files, or on any link with
          the <code>download</code> attribute, as <code>file_download</code>.
        </p>
      }
    />
  )
}

/** 404s Temps answered, with the page that most often linked to each. */
function AnsweredNotFound({
  project,
  startDate,
  endDate,
  environment,
}: AutoEventsCardProps) {
  const range =
    startDate && endDate ? notFoundWindow(startDate, endDate) : undefined

  const { data, isLoading, isError } = useQuery({
    queryKey: [
      'not-found-paths',
      project.id,
      range?.start.toISOString(),
      range?.end.toISOString(),
      environment,
    ],
    queryFn: async () => {
      if (!range) return []
      const paths = await aggregateApiTraffic({
        path: { project_id: project.id },
        body: notFoundPathsRequest(range, environment),
      })
      if (paths.error || !paths.data) {
        throw new Error(`Could not load 404 paths for project ${project.id}`)
      }
      const referrersBody = notFoundReferrersRequest(
        range,
        environment,
        paths.data.rows.flatMap((row) =>
          row.dimensions[0]?.value ? [row.dimensions[0].value] : []
        )
      )
      // A server older than the `referrer` dimension rejects this request;
      // the report then lists paths without their referrer.
      let referrers: TrafficAggregationResponse | null = null
      if (referrersBody) {
        const result = await client.post<
          { 200: TrafficAggregationResponse },
          unknown,
          false
        >({
          security: [{ scheme: 'bearer', type: 'http' }],
          url: '/projects/{project_id}/api-analytics/query',
          path: { project_id: project.id },
          body: referrersBody,
          headers: { 'Content-Type': 'application/json' },
        })
        referrers = result.error ? null : (result.data ?? null)
      }
      return mergeNotFoundRows(paths.data, referrers)
    },
    enabled: !!range,
  })

  if (isLoading) return <CardSkeleton />
  if (isError) return <LoadError what="404 responses" />
  if (!data || data.length === 0) {
    return (
      <EmptyExplainer>
        <p>
          No 404 responses to visitors in this period. This list comes from the
          requests Temps serves for this project, so it needs no tracker
          changes. A site Temps does not serve, or a single-page app that
          answers 200 for every route, reports missing pages from the browser
          instead (the other tab).
        </p>
      </EmptyExplainer>
    )
  }

  return (
    <div className="space-y-2">
      {range?.narrowed && (
        <p className="text-xs text-muted-foreground">
          Showing the last 30 days of the selected period.
        </p>
      )}
      <RankedList
        label="Paths answered with 404"
        items={data.map((row) => ({
          id: row.path,
          title: row.path,
          subtitle: row.topReferrer ? (
            <span title={row.topReferrer}>
              Linked from {compactUrl(row.topReferrer)}
            </span>
          ) : undefined,
          facts: [
            { label: 'Hits', value: row.hits.toLocaleString() },
            {
              label: 'Last seen',
              value: row.lastSeen
                ? formatDistanceToNow(new Date(row.lastSeen), {
                    addSuffix: true,
                  })
                : '—',
            },
          ],
        }))}
      />
    </div>
  )
}

/** Missing pages reported by the browser (`not_found` events). */
function ReportedNotFound({
  project,
  startDate,
  endDate,
  environment,
}: AutoEventsCardProps) {
  const { data, isLoading, isError } = useQuery({
    ...getPropertyBreakdownOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'pathname',
        event_name: NOT_FOUND_EVENT,
        environment_id: environment,
        aggregation_level: 'events',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  if (isLoading) return <CardSkeleton />
  if (isError) return <LoadError what="reported missing pages" />
  const items = data?.items ?? []
  if (items.length === 0) {
    return (
      <EmptyExplainer setupHref={`/projects/${project.slug}/analytics/setup`}>
        <p>
          No missing pages reported in this period. Add{' '}
          <code>data-temps-404</code> to any element of your “not found” view,
          or call <code>trackNotFound()</code> from it, and the tracker sends a{' '}
          <code>not_found</code> event with the path the visitor asked for.
        </p>
      </EmptyExplainer>
    )
  }
  return (
    <RankedList
      label="Paths reported as not found"
      items={items.map((item) => ({
        id: item.value || '/',
        title: item.value || '/',
        facts: [
          { label: 'Reports', value: item.count.toLocaleString() },
          { label: 'Share', value: `${item.percentage.toFixed(1)}%` },
        ],
      }))}
    />
  )
}

export function NotFoundCard(props: AutoEventsCardProps) {
  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between gap-2">
          <div>
            <CardTitle>Not found</CardTitle>
            <CardDescription>
              Pages visitors asked for that do not exist
            </CardDescription>
          </div>
          <Button variant="ghost" size="sm" className="text-xs" asChild>
            <Link to={eventDetailHref(props.project, NOT_FOUND_EVENT)}>
              Browser reports
            </Link>
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        <Tabs defaultValue="answered" className="min-w-0">
          <TabsList>
            <TabsTrigger value="answered">Answered 404</TabsTrigger>
            <TabsTrigger value="reported">Reported by the page</TabsTrigger>
          </TabsList>
          <TabsContent value="answered" className="mt-3">
            <AnsweredNotFound {...props} />
          </TabsContent>
          <TabsContent value="reported" className="mt-3">
            <ReportedNotFound {...props} />
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  )
}
