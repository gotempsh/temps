// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { Callout, PageHeader, Status } from '@temps-sdk/ds'
import { Skeleton } from '@temps-sdk/ui'
import { formatDistanceToNow } from 'date-fns'
import { Bell } from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { ClusterDnsCard } from '@/components/settings/ClusterDnsCard'
import { DnsQueryLog } from '@/components/settings/dns/DnsQueryLog'
import { DnsRecordsTable } from '@/components/settings/dns/DnsRecordsTable'
import { DnsResolversTable } from '@/components/settings/dns/DnsResolversTable'
import { DnsTestResolution } from '@/components/settings/dns/DnsTestResolution'
import { DnsTraffic } from '@/components/settings/dns/DnsTraffic'
import { ReadFailure } from '@/components/ui/read-failure'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  CONTROL_PLANE_NODE_ID,
  dnsKeys,
  getDnsResolvers,
  resolverVerdict,
  type ClusterDnsResolversResponse,
  type DnsTrafficRange,
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
        <h2 id={id} className="text-base font-semibold">
          {title}
        </h2>
        {aside}
      </div>
      {children}
    </section>
  )
}

/** Open DNS alerts, each linking to its alarm, and where they are sent. */
function OpenAlerts({ data }: { data: ClusterDnsResolversResponse }) {
  if (data.open_alerts.length === 0) return null
  const critical = data.open_alerts.some(
    (alert) => alert.severity === 'critical'
  )
  return (
    <Callout
      tone={critical ? 'error' : 'warning'}
      title={`${data.open_alerts.length} open DNS ${data.open_alerts.length === 1 ? 'alert' : 'alerts'}`}
    >
      <ul className="space-y-1">
        {data.open_alerts.map((alert) => (
          <li key={alert.id} className="[overflow-wrap:anywhere]">
            <span className="font-medium">{alert.title}</span>
            <span className="text-muted-foreground">
              {' '}
              ·{' '}
              {formatDistanceToNow(new Date(alert.fired_at), {
                addSuffix: true,
              })}
            </span>
          </li>
        ))}
      </ul>
    </Callout>
  )
}

/**
 * Settings → DNS (ADR-095): the internal resolver on every node, its
 * records, recent queries, traffic and alerts, and a name to test.
 */
export function DnsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('DNS')
  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings', href: '/settings' }, { label: 'DNS' }])
  }, [setBreadcrumbs])

  const [trafficNode, setTrafficNode] = useState(CONTROL_PLANE_NODE_ID)
  const [trafficRange, setTrafficRange] = useState<DnsTrafficRange>('1h')

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
  const verdict = data
    ? resolverVerdict(rows, data.cluster_dns_enabled)
    : undefined

  return (
    <div className="w-full min-w-0 space-y-8">
      <PageHeader title="DNS" actions={alertRouting} />

      <ClusterDnsCard />
      {data ? <OpenAlerts data={data} /> : null}

      <Section
        id="dns-resolvers"
        title="Resolvers"
        aside={
          verdict ? (
            <Status tone={verdict.tone} label={verdict.label} />
          ) : (
            <Skeleton className="h-6 w-40" />
          )
        }
      >
        <DnsResolversTable resolvers={rows} isLoading={resolvers.isPending} />
      </Section>

      <Section id="dns-test" title="Test resolution">
        {data ? (
          <DnsTestResolution resolvers={rows} />
        ) : (
          <Skeleton className="h-16 w-full" />
        )}
      </Section>

      <Section id="dns-traffic" title="Traffic">
        {data ? (
          <DnsTraffic
            resolvers={rows}
            nodeId={trafficNode}
            range={trafficRange}
            onNodeChange={setTrafficNode}
            onRangeChange={setTrafficRange}
          />
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
        <DnsQueryLog resolvers={rows} />
      </Section>

      <Section
        id="dns-records"
        title="Records"
        aside={
          data ? (
            <span className="text-xs text-muted-foreground tabular-nums">
              {data.zone_record_count.toLocaleString()} in the zone
            </span>
          ) : undefined
        }
      >
        <DnsRecordsTable resolvers={rows} />
      </Section>
    </div>
  )
}
