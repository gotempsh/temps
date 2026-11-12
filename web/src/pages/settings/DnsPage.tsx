// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import {
  PageHeader,
  Status,
  StatusDot,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  useUrlState,
} from '@temps-sdk/ds'
import { Skeleton } from '@temps-sdk/ui'
import { Bell } from 'lucide-react'
import { useEffect, type ReactNode } from 'react'
import { Link } from 'react-router'
import { DnsOverview } from '@/components/settings/dns/DnsOverview'
import { DnsQueryLog } from '@/components/settings/dns/DnsQueryLog'
import { DnsRecordsTable } from '@/components/settings/dns/DnsRecordsTable'
import { DnsTestResolution } from '@/components/settings/dns/DnsTestResolution'
import { DnsTraffic } from '@/components/settings/dns/DnsTraffic'
import { ReadFailure } from '@/components/ui/read-failure'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  DNS_PARAMS,
  dnsKeys,
  dnsSummary,
  getDnsResolvers,
  parseDnsTab,
  withParams,
  type DnsParam,
  type DnsSummary,
  type DnsTab,
} from '@/lib/dns-observability'
import { isForbiddenRead } from '@/lib/read-failure'

function Section({
  id,
  title,
  aside,
  children,
}: {
  id: string
  title: string
  aside?: ReactNode
  children: ReactNode
}) {
  return (
    <section aria-labelledby={id} className="space-y-3">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h2 id={id} className="text-sm font-medium">
          {title}
        </h2>
        {aside}
      </div>
      {children}
    </section>
  )
}

/**
 * The header's one-glance verdict. When nodes are affected it links to the
 * first one's row in Overview, opened.
 */
function SummaryBadge({
  summary,
  search,
}: {
  summary: DnsSummary | undefined
  search: URLSearchParams
}) {
  if (!summary) return <Skeleton className="h-6 w-44" />
  const badge = <Status tone={summary.tone} label={summary.label} />
  const first = summary.affected[0]
  if (!first) return badge
  return (
    <Link
      to={withParams(search, { tab: undefined, node: first.nodeId })}
      aria-label={`${summary.label}: show ${first.nodeName}`}
      className="w-fit rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      {badge}
    </Link>
  )
}

/**
 * Settings → DNS (ADR-095), in four tabs whose choice lives in `?tab=`:
 * Overview (is internal DNS working, and what to do if not), Records,
 * Queries (traffic and the query log) and Test resolution.
 */
export function DnsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('DNS')
  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings', href: '/settings' }, { label: 'DNS' }])
  }, [setBreadcrumbs])

  const { get, patch, params: search } = useUrlState<DnsParam>()
  const tab = parseDnsTab(get(DNS_PARAMS.tab))
  const onTabChange = (value: string) => {
    const next: DnsTab = parseDnsTab(value)
    // Each tab is a history entry, so Back returns to the previous one.
    patch(
      { [DNS_PARAMS.tab]: next === 'overview' ? undefined : next },
      { replace: false }
    )
  }

  const resolvers = useQuery({
    queryKey: dnsKeys.resolvers,
    queryFn: getDnsResolvers,
    refetchInterval: 30_000,
    meta: { errorTitle: 'Could not load the DNS resolvers' },
  })

  const alertRouting = (
    <Link
      to="/settings/notifications"
      className="inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
    >
      <Bell className="size-4" aria-hidden />
      Where DNS alerts are sent
    </Link>
  )

  if (resolvers.isError) {
    return (
      <div className="w-full min-w-0 space-y-6">
        <PageHeader title="DNS" />
        <ReadFailure
          resource="DNS resolvers"
          error={resolvers.error}
          onRetry={
            isForbiddenRead(resolvers.error)
              ? undefined
              : () => void resolvers.refetch()
          }
          retrying={resolvers.isFetching}
        />
      </div>
    )
  }

  const data = resolvers.data
  const rows = data?.resolvers ?? []
  const enabled = data?.cluster_dns_enabled ?? true
  const summary = data ? dnsSummary(rows, data.cluster_dns_enabled) : undefined
  const alarming = summary?.tone === 'error' || summary?.tone === 'warn'

  return (
    <div className="w-full min-w-0 space-y-4">
      <PageHeader
        title="DNS"
        verdict={<SummaryBadge summary={summary} search={search} />}
        actions={alertRouting}
      />

      <Tabs value={tab} onValueChange={onTabChange}>
        <TabsList aria-label="DNS views" className="flex-wrap">
          <TabsTrigger value="overview">
            Overview
            {alarming && summary ? <StatusDot tone={summary.tone} /> : null}
          </TabsTrigger>
          <TabsTrigger value="records" count={data?.zone_record_count}>
            Records
          </TabsTrigger>
          <TabsTrigger value="queries">Queries</TabsTrigger>
          <TabsTrigger value="test">
            {/* One row of tabs on a phone: the short name below `sm`. */}
            <span className="sm:hidden">Test</span>
            <span className="max-sm:hidden">Test resolution</span>
          </TabsTrigger>
        </TabsList>

        <TabsContent value="overview">
          <DnsOverview data={data} summary={summary} search={search} />
        </TabsContent>

        <TabsContent value="records">
          <DnsRecordsTable resolvers={rows} />
        </TabsContent>

        <TabsContent value="queries" className="space-y-8">
          <Section id="dns-traffic" title="Traffic">
            {data ? (
              <DnsTraffic resolvers={rows} />
            ) : (
              <Skeleton className="h-64 w-full" />
            )}
          </Section>
          <Section
            id="dns-queries"
            title="Recent queries"
            aside={
              <span className="text-xs text-muted-foreground">
                Kept 24 hours · forwarded names are not logged
              </span>
            }
          >
            <DnsQueryLog resolvers={rows} enabled={enabled} />
          </Section>
        </TabsContent>

        <TabsContent value="test">
          {data ? (
            <DnsTestResolution resolvers={rows} />
          ) : (
            <Skeleton className="h-16 w-full" />
          )}
        </TabsContent>
      </Tabs>
    </div>
  )
}
