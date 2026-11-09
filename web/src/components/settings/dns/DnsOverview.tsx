// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Disclosure, Status, StatusDot } from '@temps-sdk/ds'
import { Button, Skeleton } from '@temps-sdk/ui'
import { formatDistanceToNow } from 'date-fns'
import { ChevronRight } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { Link } from 'react-router'
import { ClusterDnsCard } from '@/components/settings/ClusterDnsCard'
import {
  DNS_PARAMS,
  formatAge,
  nodeRowId,
  withParams,
  type ClusterDnsResolversResponse,
  type DnsFacet,
  type DnsResolverEntry,
  type DnsSummary,
  type NodeDnsHealth,
} from '@/lib/dns-observability'
import { cn } from '@/lib/utils'

function reportedAgo(at: string | null | undefined): string {
  if (!at) return 'Never'
  return formatDistanceToNow(new Date(at), { addSuffix: true })
}

/**
 * One row of the node's two facets: "Internal routing ● Down — <reason>".
 * The reason is the API's text, shown as reported.
 */
function Facet({ name, facet }: { name: string; facet: DnsFacet | null }) {
  return (
    <>
      <dt className="text-muted-foreground">{name}</dt>
      <dd className="flex min-w-0 items-baseline gap-1.5">
        {facet ? (
          <>
            <StatusDot tone={facet.tone} className="translate-y-[-1px]" />
            <span
              className={cn(
                '[overflow-wrap:anywhere]',
                facet.tone === 'error' && 'text-destructive'
              )}
            >
              <span className="font-medium">{facet.label}</span>
              {facet.reason ? ` — ${facet.reason}` : null}
            </span>
          </>
        ) : (
          <span className="text-muted-foreground">Main proxy</span>
        )}
      </dd>
    </>
  )
}

function Fact({ label, children }: { label: string; children: string }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm [overflow-wrap:anywhere]">{children}</dd>
    </div>
  )
}

/** What the old table's secondary columns said, for the open row. */
function NodeDetails({
  resolver,
  search,
}: {
  resolver: DnsResolverEntry
  search: URLSearchParams
}) {
  const failures = resolver.consecutive_sync_failures
  const sync =
    resolver.is_control_plane && resolver.last_sync_age_secs == null
      ? 'Fed locally'
      : formatAge(resolver.last_sync_age_secs)
  return (
    <div className="space-y-4 border-t bg-muted/30 px-4 py-3 sm:pl-11">
      <dl className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-4">
        <Fact label="Listens on">{resolver.listen_address ?? '—'}</Fact>
        <Fact label="Internal routing on">
          {resolver.internal_proxy
            ? (resolver.internal_proxy.address ?? '—')
            : 'Main proxy'}
        </Fact>
        <Fact label="Version">{resolver.build_version ?? '—'}</Fact>
        <Fact label="Records served">
          {resolver.record_count?.toLocaleString() ?? '—'}
        </Fact>
        <Fact label="Last record sync">
          {failures > 0
            ? `${sync} · ${failures} failed ${failures === 1 ? 'sync' : 'syncs'} in a row`
            : sync}
        </Fact>
        <Fact label="Last report">{reportedAgo(resolver.reported_at)}</Fact>
        {resolver.dropped_log_entries > 0 ? (
          <Fact label="Log entries dropped under load">
            {resolver.dropped_log_entries.toLocaleString()}
          </Fact>
        ) : null}
        {resolver.last_error ? (
          <Fact label="Last error">{resolver.last_error}</Fact>
        ) : null}
      </dl>
      <div className="flex flex-wrap gap-2">
        <Button asChild size="sm" variant="outline">
          <Link
            to={withParams(search, {
              tab: 'test',
              from: resolver.node_id,
            })}
          >
            Test a name from {resolver.node_name}
          </Link>
        </Button>
        <Button asChild size="sm" variant="outline">
          <Link
            to={withParams(search, {
              tab: 'queries',
              qnode: resolver.node_id,
              tnode: resolver.node_id,
            })}
          >
            Queries and traffic
          </Link>
        </Button>
      </div>
    </div>
  )
}

function NodeRow({
  resolver,
  health,
  open,
  search,
}: {
  resolver: DnsResolverEntry
  health: NodeDnsHealth
  open: boolean
  search: URLSearchParams
}) {
  const ref = useRef<HTMLLIElement>(null)
  // The header badge and alerts link here: bring the row into view.
  useEffect(() => {
    if (open) ref.current?.scrollIntoView?.({ block: 'nearest' })
  }, [open])
  const detailsId = `${nodeRowId(resolver.node_id)}-details`
  const toggle = withParams(search, {
    node: open ? undefined : resolver.node_id,
  })

  return (
    <li
      ref={ref}
      id={nodeRowId(resolver.node_id)}
      className={cn('scroll-mt-4', open && 'bg-muted/20')}
    >
      <div className="flex items-start gap-3 px-4 py-3">
        <Button
          asChild
          variant="ghost"
          size="icon"
          className="-ml-2 size-7 shrink-0 text-muted-foreground"
        >
          <Link
            to={toggle}
            replace
            aria-expanded={open}
            aria-controls={detailsId}
            aria-label={`${open ? 'Hide' : 'Show'} details for ${resolver.node_name}`}
          >
            <ChevronRight
              aria-hidden
              className={cn('size-4', open && 'rotate-90')}
            />
          </Link>
        </Button>
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="flex flex-wrap items-center justify-between gap-2">
            {resolver.is_control_plane ? (
              <span className="font-medium [overflow-wrap:anywhere]">
                {resolver.node_name}
              </span>
            ) : (
              <Link
                to={`/settings/nodes/${resolver.node_id}`}
                className="font-medium hover:underline [overflow-wrap:anywhere]"
              >
                {resolver.node_name}
              </Link>
            )}
            <Status tone={health.tone} label={health.label} />
          </div>

          {health.note ? (
            <p className="text-sm text-muted-foreground">{health.note}</p>
          ) : (
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
              <Facet name="Name lookups" facet={health.lookups} />
              <Facet name="Internal routing" facet={health.routing} />
            </dl>
          )}

          {(health.problem?.why ?? health.noteWhy) ? (
            <Disclosure label="Why?">
              <p className="max-w-prose [overflow-wrap:anywhere]">
                {health.problem?.why ?? health.noteWhy}
              </p>
            </Disclosure>
          ) : null}
        </div>
      </div>
      {open ? (
        <div id={detailsId}>
          <NodeDetails resolver={resolver} search={search} />
        </div>
      ) : null}
    </li>
  )
}

/**
 * The one-glance answer under the header's badge: what it means for apps,
 * a way to the first affected node, and the open alerts behind it.
 */
function Summary({
  data,
  summary,
  search,
}: {
  data: ClusterDnsResolversResponse
  summary: DnsSummary
  search: URLSearchParams
}) {
  const first = summary.affected[0]
  return (
    <div
      className={cn(
        'space-y-2 rounded-md border px-4 py-3',
        summary.tone === 'error' && 'border-destructive/40 bg-destructive/5',
        summary.tone === 'warn' && 'border-warning/40 bg-warning/5'
      )}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="flex items-center gap-2 text-sm font-medium">
          <StatusDot tone={summary.tone} />
          {summary.detail}
        </p>
        {first ? (
          <Button asChild size="sm" variant="outline">
            <Link
              to={withParams(search, { tab: undefined, node: first.nodeId })}
            >
              Show {first.nodeName}
            </Link>
          </Button>
        ) : null}
      </div>
      {data.open_alerts.length > 0 ? (
        <ul
          aria-label="Open alerts"
          className="space-y-0.5 text-xs text-muted-foreground"
        >
          {data.open_alerts.map((alert) => (
            <li key={alert.id} className="[overflow-wrap:anywhere]">
              Alert: {alert.title} ·{' '}
              {formatDistanceToNow(new Date(alert.fired_at), {
                addSuffix: true,
              })}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}

function SummarySkeleton() {
  return (
    <div className="space-y-4">
      <Skeleton className="h-[72px] w-full" />
      <div className="divide-y rounded-md border">
        {Array.from({ length: 2 }).map((_, index) => (
          <div key={index} className="space-y-2 px-4 py-3">
            <Skeleton className="h-5 w-48" />
            <Skeleton className="h-4 w-72" />
          </div>
        ))}
      </div>
    </div>
  )
}

/**
 * Is internal DNS working, and if not, what to do: one summary, then one
 * row per node with a single health, the reason leading and the long
 * explanation behind "Why?". The cluster-DNS switch closes the tab.
 */
export function DnsOverview({
  data,
  summary,
  search,
}: {
  data: ClusterDnsResolversResponse | undefined
  summary: DnsSummary | undefined
  search: URLSearchParams
}) {
  const openNode = search.get(DNS_PARAMS.node)

  return (
    <div className="space-y-6">
      {data && summary ? (
        <>
          <Summary data={data} summary={summary} search={search} />

          <section aria-labelledby="dns-nodes" className="space-y-2">
            <h2 id="dns-nodes" className="text-sm font-medium">
              Nodes
            </h2>
            <ul className="divide-y rounded-md border">
              {data.resolvers.map((resolver, index) => (
                <NodeRow
                  key={resolver.node_id}
                  resolver={resolver}
                  health={summary.nodes[index]}
                  open={openNode === String(resolver.node_id)}
                  search={search}
                />
              ))}
            </ul>
          </section>
        </>
      ) : (
        <SummarySkeleton />
      )}

      <ClusterDnsCard />
    </div>
  )
}
