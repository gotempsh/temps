// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { OBSERVABILITY_PAGE_SIZE } from '@/lib/global-observability'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { getGlobalAnalyticsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  AnalyticsTrafficChart,
  type AnalyticsMetric,
} from '@/components/analytics/overview/AnalyticsTrafficChart'
import { AnalyticsSummary } from '@/components/analytics/overview/AnalyticsMetrics'
import {
  AnalyticsDimensionIcon,
  dimensionLabel,
} from '@/components/analytics/overview/AnalyticsDimensionIdentity'
import { AnalyticsBreakdownRow } from '@/components/analytics/overview/AnalyticsBreakdownRow'
import { AnalyticsBreakdowns } from '@/components/analytics/overview/AnalyticsBreakdowns'
import { DataSection } from '@/components/data-display/DataSection'
import {
  GlobalPage,
  QueryContent,
  GlobalPagination,
} from '@/components/observability/GlobalPage'
import { useGlobalView } from '@/hooks/useGlobalView'
import type { GlobalView } from '@/hooks/useGlobalView'
import type { AnalyticsFacet } from '@/api/client/types.gen'
import { EmptyState } from '@/components/ui/empty-state'
import { BarChart3 } from 'lucide-react'
import { Link } from 'react-router'

function Breakdown({
  title,
  view,
  facet = 'breakdown',
  dimension,
  totalVisitors,
  expanded = false,
}: {
  title: string
  view: GlobalView
  facet?: AnalyticsFacet
  dimension?: string
  totalVisitors: number
  expanded?: boolean
}) {
  const query = useQuery(
    getGlobalAnalyticsOptions({
      query: {
        project_id: view.projectId,
        start_date: view.from,
        end_date: view.to,
        facet,
        dimension,
        sort_by: 'visitors',
        sort_order: 'desc',
        per_page: expanded ? OBSERVABILITY_PAGE_SIZE : 10,
        search: view.search || undefined,
        page: expanded ? view.page : 1,
      },
    })
  )
  return (
    <DataSection
      title={title}
      actions={
        <Button
          variant="ghost"
          size="sm"
          onClick={() =>
            view.patch({
              breakdown: expanded ? undefined : (dimension ?? facet),
              page: undefined,
            })
          }
        >
          {expanded ? 'Back to overview' : 'View all'}
        </Button>
      }
    >
      <QueryContent
        title={title}
        loading={query.isPending}
        error={query.error}
        empty={!query.data?.rows.length}
        retry={() => void query.refetch()}
      >
        <div className="space-y-1" aria-label={`${title} by visitors`}>
          {(query.data?.rows ?? []).map((row) => (
            <AnalyticsBreakdownRow
              key={`${row.project_id}:${row.key}`}
              label={dimensionLabel(dimension, row.key)}
              icon={
                <AnalyticsDimensionIcon
                  dimension={facet === 'events' ? 'event' : dimension}
                  value={row.key}
                />
              }
              count={row.visitors}
              percentage={
                totalVisitors > 0 ? (row.visitors / totalVisitors) * 100 : 0
              }
              subtitle={
                view.projectId ? undefined : row.project_name || undefined
              }
            />
          ))}
        </div>
        <p className="mt-4 text-xs text-muted-foreground">
          Share of all visitors in the selected scope
        </p>
        {expanded && (
          <GlobalPagination view={view} total={query.data?.total ?? 0} />
        )}
      </QueryContent>
    </DataSection>
  )
}
export default function GlobalAnalytics() {
  const view = useGlobalView()
  const queryClient = useQueryClient()
  const [metric, setMetric] = useState<AnalyticsMetric>('visitors')
  const common = {
    project_id: view.projectId,
    start_date: view.from,
    end_date: view.to,
  }
  const summary = useQuery(
    getGlobalAnalyticsOptions({ query: { ...common, facet: 'summary' } })
  )
  const traffic = useQuery(
    getGlobalAnalyticsOptions({ query: { ...common, facet: 'traffic' } })
  )
  const pages = useQuery(
    getGlobalAnalyticsOptions({
      query: { ...common, facet: 'pages', per_page: 1 },
    })
  )
  const totals = summary.data?.rows[0]
  const grid = (children: ReactNode) => (
    <div className="grid min-w-0 grid-cols-1 gap-4 md:grid-cols-2">
      {children}
    </div>
  )
  const breakdown = (title: string, dimension: string) => (
    <Breakdown
      title={title}
      dimension={dimension}
      view={view}
      totalVisitors={totals?.visitors ?? 0}
    />
  )
  const selectedBreakdown = view.params.get('breakdown')
  const breakdowns: Record<string, string> = {
    pages: 'Pages',
    referrer_hostname: 'Referrers',
    channel: 'Channels',
    utm_campaign: 'UTM Campaigns',
    country: 'Locations',
    language: 'Languages',
    browser: 'Browsers',
    operating_system: 'Operating Systems',
    device_type: 'Devices',
    events: 'Events',
  }
  const expandedTitle = selectedBreakdown
    ? breakdowns[selectedBreakdown]
    : undefined
  const hasNoAnalytics =
    !summary.isPending &&
    !traffic.isPending &&
    !pages.isPending &&
    (totals?.visitors ?? 0) === 0 &&
    (totals?.sessions ?? 0) === 0 &&
    (pages.data?.total ?? 0) === 0 &&
    (traffic.data?.rows.length ?? 0) === 0
  return (
    <GlobalPage
      title="Analytics"
      description="Traffic, audience, and engagement across your projects."
      view={view}
      fetching={summary.isFetching || traffic.isFetching || pages.isFetching}
      refresh={() => {
        void queryClient.invalidateQueries({
          predicate: (query) =>
            (query.queryKey[0] as { _id?: string })?._id ===
            'getGlobalAnalytics',
        })
      }}
      searchLabel="Search analytics breakdowns"
    >
      {expandedTitle && selectedBreakdown ? (
        <Breakdown
          key={selectedBreakdown}
          title={expandedTitle}
          view={view}
          facet={
            selectedBreakdown === 'pages' || selectedBreakdown === 'events'
              ? selectedBreakdown
              : 'breakdown'
          }
          dimension={
            selectedBreakdown === 'pages' || selectedBreakdown === 'events'
              ? undefined
              : selectedBreakdown
          }
          totalVisitors={totals?.visitors ?? 0}
          expanded
        />
      ) : (
        <>
          <QueryContent
            title="Analytics metrics"
            loading={summary.isPending || pages.isPending}
            error={summary.error || pages.error}
            empty={false}
            retry={() => {
              void summary.refetch()
              void pages.refetch()
            }}
          >
            <AnalyticsSummary
              visitors={totals?.visitors ?? 0}
              sessions={totals?.sessions ?? 0}
              pages={pages.data?.total ?? 0}
            />
          </QueryContent>
          {hasNoAnalytics ? (
            <DataSection title="Traffic">
              <EmptyState
                size="compact"
                icon={BarChart3}
                title="No analytics data yet"
                description="Open a project to finish analytics setup and start collecting page views, sessions, and visitors."
                action={
                  <Button asChild size="sm">
                    <Link to="/projects">Open projects</Link>
                  </Button>
                }
              />
            </DataSection>
          ) : (
            <AnalyticsTrafficChart
              data={traffic.data?.rows.map((row) => ({
                date: row.key,
                count:
                  metric === 'events'
                    ? row.views
                    : metric === 'sessions'
                      ? row.sessions
                      : row.visitors,
              }))}
              startDate={new Date(view.from)}
              endDate={new Date(view.to)}
              isLoading={traffic.isPending}
              error={traffic.error}
              aggregationLevel={metric}
              onAggregationChange={setMetric}
              onZoom={(from, to) =>
                view.setTimeRange({
                  from: from.toISOString(),
                  to: to.toISOString(),
                  preset: 'custom',
                })
              }
            />
          )}
          {!hasNoAnalytics && (
            <AnalyticsBreakdowns
              traffic={grid(
                <>
                  <Breakdown
                    title="Top Pages"
                    facet="pages"
                    view={view}
                    totalVisitors={totals?.visitors ?? 0}
                  />
                  {breakdown('Referrers', 'referrer_hostname')}
                  {breakdown('Channels', 'channel')}
                  {breakdown('UTM Campaigns', 'utm_campaign')}
                </>
              )}
              audience={grid(
                <>
                  {breakdown('Locations', 'country')}
                  {breakdown('Languages', 'language')}
                </>
              )}
              technology={grid(
                <>
                  {breakdown('Browsers', 'browser')}
                  {breakdown('Operating Systems', 'operating_system')}
                  {breakdown('Devices', 'device_type')}
                </>
              )}
              events={grid(
                <Breakdown
                  title="Events"
                  facet="events"
                  view={view}
                  totalVisitors={totals?.visitors ?? 0}
                />
              )}
            />
          )}
        </>
      )}
    </GlobalPage>
  )
}
