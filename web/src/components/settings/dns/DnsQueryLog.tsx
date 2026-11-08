// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { keepPreviousData, useQuery } from '@tanstack/react-query'
import {
  DataTable,
  PageState,
  Status,
  type DataTableColumn,
} from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import { format } from 'date-fns'
import { ScrollText } from 'lucide-react'
import { useState } from 'react'
import { ReadFailure } from '@/components/ui/read-failure'
import { useDebounce } from '@/hooks/useDebounce'
import {
  askedBy,
  DNS_OUTCOMES,
  dnsKeys,
  formatLatencyUs,
  getDnsQueries,
  outcomeLabel,
  outcomeTone,
  type DnsOutcome,
  type DnsQueriesParams,
  type DnsQueryLogEntry,
  type DnsResolverEntry,
  type DnsZone,
} from '@/lib/dns-observability'
import { isForbiddenRead } from '@/lib/read-failure'

const ALL = 'all'
const PAGE_SIZE_OPTIONS = [20, 50, 100] as const

function NameCell({ entry }: { entry: DnsQueryLogEntry }) {
  const who = askedBy(entry)
  return (
    <div className="min-w-0 space-y-0.5">
      <p className="font-mono text-xs [overflow-wrap:anywhere]">
        {entry.qname ?? (
          <span className="font-sans text-muted-foreground">
            Forwarded name (not logged)
          </span>
        )}
        <span className="ml-1.5 text-muted-foreground">{entry.qtype}</span>
      </p>
      {/* What the hidden columns say, for a narrow table. */}
      <p className="text-xs text-muted-foreground @2xl:hidden">
        {who.primary}
        {who.secondary ? ` / ${who.secondary}` : ''} ·{' '}
        {formatLatencyUs(entry.latency_us)}
      </p>
    </div>
  )
}

const columns: DataTableColumn<DnsQueryLogEntry>[] = [
  {
    key: 'time',
    header: 'Time',
    className: 'hidden @xl:table-cell whitespace-nowrap text-muted-foreground',
    render: (entry) => format(new Date(entry.observed_at), 'HH:mm:ss'),
  },
  {
    key: 'name',
    header: 'Name',
    render: (entry) => <NameCell entry={entry} />,
  },
  {
    key: 'outcome',
    header: 'Outcome',
    render: (entry) => (
      <Status
        variant="dot"
        tone={outcomeTone(entry.outcome)}
        label={outcomeLabel(entry.outcome)}
      />
    ),
  },
  {
    key: 'latency',
    header: 'Latency',
    className: 'hidden @2xl:table-cell tabular-nums',
    render: (entry) => formatLatencyUs(entry.latency_us),
  },
  {
    key: 'asked-by',
    header: 'Asked by',
    className: 'hidden @2xl:table-cell',
    render: (entry) => {
      const who = askedBy(entry)
      return (
        <div className="min-w-0">
          <p className="[overflow-wrap:anywhere]">{who.primary}</p>
          {who.secondary ? (
            <p className="text-xs text-muted-foreground">{who.secondary}</p>
          ) : null}
          {who.primary !== entry.source_ip ? (
            <p className="font-mono text-xs text-muted-foreground">
              {entry.source_ip}
            </p>
          ) : null}
        </div>
      )
    },
  },
  {
    key: 'node',
    header: 'Node',
    className: 'hidden @4xl:table-cell',
    render: (entry) => entry.node_name,
  },
]

/** Recent queries, newest first, filterable by node, outcome, zone and name. */
export function DnsQueryLog({ resolvers }: { resolvers: DnsResolverEntry[] }) {
  const [params, setParams] = useState<DnsQueriesParams>({
    page: 1,
    page_size: 20,
  })
  const [name, setName] = useState('')
  const debouncedName = useDebounce(name.trim(), 300)
  const update = (patch: Partial<DnsQueriesParams>) =>
    setParams((current) => ({ ...current, page: 1, ...patch }))
  const request: DnsQueriesParams = {
    ...params,
    name: debouncedName || undefined,
  }

  const query = useQuery({
    queryKey: dnsKeys.queries(request),
    queryFn: () => getDnsQueries(request),
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
    meta: { errorTitle: 'Could not load the DNS query log' },
  })

  const total = query.data?.total ?? 0
  const filtered =
    params.node_id !== undefined ||
    params.outcome !== undefined ||
    params.zone !== undefined ||
    Boolean(request.name)

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
        <Input
          aria-label="Name contains"
          placeholder="Name contains…"
          className="w-full sm:w-[240px]"
          value={name}
          onChange={(event) => {
            setName(event.target.value)
            setParams((current) => ({ ...current, page: 1 }))
          }}
        />
        <Select
          value={params.node_id === undefined ? ALL : String(params.node_id)}
          onValueChange={(value) =>
            update({ node_id: value === ALL ? undefined : Number(value) })
          }
        >
          <SelectTrigger className="w-full sm:w-[200px]" aria-label="Node">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All nodes</SelectItem>
            {resolvers.map((resolver) => (
              <SelectItem
                key={resolver.node_id}
                value={String(resolver.node_id)}
              >
                {resolver.node_name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          value={params.outcome ?? ALL}
          onValueChange={(value) =>
            update({
              outcome: value === ALL ? undefined : (value as DnsOutcome),
            })
          }
        >
          <SelectTrigger className="w-full sm:w-[180px]" aria-label="Outcome">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All outcomes</SelectItem>
            {DNS_OUTCOMES.map((outcome) => (
              <SelectItem key={outcome} value={outcome}>
                {outcomeLabel(outcome)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          value={params.zone ?? ALL}
          onValueChange={(value) =>
            update({ zone: value === ALL ? undefined : (value as DnsZone) })
          }
        >
          <SelectTrigger className="w-full sm:w-[200px]" aria-label="Zone">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>Internal and forwarded</SelectItem>
            <SelectItem value="internal">*.temps.local</SelectItem>
            <SelectItem value="external">Forwarded</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {query.isError ? (
        <ReadFailure
          resource="DNS query log"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : !query.isPending && total === 0 ? (
        <PageState
          variant="empty"
          size="compact"
          icon={ScrollText}
          title={filtered ? 'No queries match these filters' : 'No queries yet'}
          description={
            filtered
              ? undefined
              : `Queries for *.temps.local names, and failed forwarded queries, appear here and are kept for ${query.data?.retention_hours ?? 24} hours.`
          }
        />
      ) : (
        <DataTable
          aria-label="DNS queries"
          columns={columns}
          rows={query.data?.items ?? []}
          rowKey={(entry) => entry.id}
          isLoading={query.isPending}
          pagination={{
            page: params.page,
            pageSize: params.page_size,
            total,
            totalPages: Math.max(1, Math.ceil(total / params.page_size)),
            onPageChange: (page) =>
              setParams((current) => ({ ...current, page })),
            onPageSizeChange: (pageSize) => update({ page_size: pageSize }),
            pageSizeOptions: PAGE_SIZE_OPTIONS,
            ariaLabel: 'DNS query log pages',
          }}
        />
      )}
    </div>
  )
}
