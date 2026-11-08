// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { DataTable, Status, type DataTableColumn } from '@temps-sdk/ds'
import { formatDistanceToNow } from 'date-fns'
import { Link } from 'react-router'
import {
  formatAge,
  internalProxyMeta,
  resolverHealthMeta,
  type DnsResolverEntry,
} from '@/lib/dns-observability'

function reportedAgo(at: string | null | undefined): string {
  if (!at) return 'Never'
  return formatDistanceToNow(new Date(at), { addSuffix: true })
}

function NodeCell({ resolver }: { resolver: DnsResolverEntry }) {
  return (
    <div className="min-w-0 space-y-1">
      {resolver.is_control_plane ? (
        <span className="font-medium">{resolver.node_name}</span>
      ) : (
        <Link
          to={`/settings/nodes/${resolver.node_id}`}
          className="font-medium hover:underline [overflow-wrap:anywhere]"
        >
          {resolver.node_name}
        </Link>
      )}
      {resolver.health_reason ? (
        <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
          {resolver.health_reason}
        </p>
      ) : null}
      {resolver.internal_proxy?.listening === false ? (
        <p className="text-xs text-destructive [overflow-wrap:anywhere]">
          Internal routing down on{' '}
          {resolver.internal_proxy.address ?? 'its bridge address'}:{' '}
          {resolver.internal_proxy.error ?? 'no error reported'}
        </p>
      ) : null}
      {/* What the hidden columns say, for a narrow table. */}
      <p className="text-xs text-muted-foreground @xl:hidden">
        Synced {formatAge(resolver.last_sync_age_secs)}
        {resolver.record_count !== null && resolver.record_count !== undefined
          ? ` · ${resolver.record_count} records`
          : ''}
      </p>
    </div>
  )
}

const columns: DataTableColumn<DnsResolverEntry>[] = [
  {
    key: 'node',
    header: 'Node',
    render: (resolver) => <NodeCell resolver={resolver} />,
  },
  {
    key: 'health',
    header: 'Resolver',
    render: (resolver) => {
      const meta = resolverHealthMeta(resolver.health)
      return <Status tone={meta.tone} label={meta.label} />
    },
  },
  {
    key: 'routing',
    header: 'Internal routing',
    className: 'hidden @2xl:table-cell',
    render: (resolver) => {
      const meta = internalProxyMeta(resolver.internal_proxy)
      if (!meta)
        return <span className="text-muted-foreground">Main proxy</span>
      return (
        <span className="space-y-0.5">
          <Status variant="dot" tone={meta.tone} label={meta.label} />
          {resolver.internal_proxy?.address ? (
            <span className="block font-mono text-xs text-muted-foreground">
              {resolver.internal_proxy.address}
            </span>
          ) : null}
        </span>
      )
    },
  },
  {
    key: 'records',
    header: 'Records',
    className: 'hidden @xl:table-cell tabular-nums',
    render: (resolver) => resolver.record_count ?? '—',
  },
  {
    key: 'sync',
    header: 'Last record sync',
    className: 'hidden @xl:table-cell',
    render: (resolver) => (
      <span>
        {formatAge(resolver.last_sync_age_secs)}
        {resolver.consecutive_sync_failures > 0 ? (
          <span className="block text-xs text-muted-foreground">
            {resolver.consecutive_sync_failures} failed{' '}
            {resolver.consecutive_sync_failures === 1 ? 'sync' : 'syncs'} in a
            row
          </span>
        ) : null}
      </span>
    ),
  },
  {
    key: 'version',
    header: 'Version',
    className: 'hidden @3xl:table-cell font-mono text-xs',
    render: (resolver) => resolver.build_version ?? '—',
  },
  {
    key: 'listen',
    header: 'Listens on',
    className: 'hidden @4xl:table-cell font-mono text-xs',
    render: (resolver) => resolver.listen_address ?? '—',
  },
  {
    key: 'reported',
    header: 'Last report',
    className: 'hidden @3xl:table-cell text-muted-foreground',
    render: (resolver) => reportedAgo(resolver.reported_at),
  },
]

/** Resolver health, one row per node: the control plane first. */
export function DnsResolversTable({
  resolvers,
  isLoading,
}: {
  resolvers: DnsResolverEntry[]
  isLoading?: boolean
}) {
  return (
    <DataTable
      aria-label="Resolvers"
      columns={columns}
      rows={resolvers}
      rowKey={(resolver) => resolver.node_id}
      isLoading={isLoading}
    />
  )
}
