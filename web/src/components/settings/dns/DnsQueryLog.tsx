// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { keepPreviousData, useQuery } from '@tanstack/react-query'
import {
  DataTable,
  PageState,
  Status,
  useUrlSearchText,
  useUrlState,
  type DataTableColumn,
} from '@temps-sdk/ds'
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import { format } from 'date-fns'
import { ScrollText } from 'lucide-react'
import { Link } from 'react-router'
import { ReadFailure } from '@/components/ui/read-failure'
import { useDebounce } from '@/hooks/useDebounce'
import {
  askedBy,
  DNS_OUTCOMES,
  DNS_PAGE_SIZES,
  DNS_PARAMS,
  DNS_ZONES,
  dnsKeys,
  formatLatencyUs,
  getDnsQueries,
  outcomeLabel,
  parseIntParam,
  parsePageSize,
  withParams,
  outcomeTone,
  type DnsOutcome,
  type DnsParam,
  type DnsQueriesParams,
  type DnsQueryLogEntry,
  type DnsResolverEntry,
  type DnsZone,
} from '@/lib/dns-observability'
import { isForbiddenRead } from '@/lib/read-failure'

const ALL = 'all'

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
    header: 'Result',
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

function parseOutcome(value: string | null): DnsOutcome | undefined {
  return (DNS_OUTCOMES as readonly string[]).includes(value ?? '')
    ? (value as DnsOutcome)
    : undefined
}

function parseZone(value: string | null): DnsZone | undefined {
  return (DNS_ZONES as readonly string[]).includes(value ?? '')
    ? (value as DnsZone)
    : undefined
}

/**
 * Recent queries, newest first, filterable by node, result, zone and name.
 * The filters live in the URL, so "NXDOMAIN on worker-a" is a link.
 */
export function DnsQueryLog({
  resolvers,
  enabled,
}: {
  resolvers: DnsResolverEntry[]
  enabled: boolean
}) {
  const urlState = useUrlState<DnsParam>()
  const { get, patch, params: search } = urlState
  const [name, setName] = useUrlSearchText(DNS_PARAMS.queryName, {
    resetKeys: [DNS_PARAMS.queryPage],
    urlState,
  })
  const debouncedName = useDebounce(name.trim(), 300)
  const page = Math.max(1, parseIntParam(get(DNS_PARAMS.queryPage)) ?? 1)
  const pageSize = parsePageSize(get(DNS_PARAMS.queryPageSize))
  const request: DnsQueriesParams = {
    page,
    page_size: pageSize,
    node_id: parseIntParam(get(DNS_PARAMS.queryNode)),
    outcome: parseOutcome(get(DNS_PARAMS.queryOutcome)),
    zone: parseZone(get(DNS_PARAMS.queryZone)),
    name: debouncedName || undefined,
  }
  const update = (values: Partial<Record<DnsParam, string | number>>) =>
    patch({ [DNS_PARAMS.queryPage]: undefined, ...values })

  const query = useQuery({
    queryKey: dnsKeys.queries(request),
    queryFn: () => getDnsQueries(request),
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
    meta: { errorTitle: 'Could not load the DNS query log' },
  })

  const total = query.data?.total ?? 0
  const filtered =
    request.node_id !== undefined ||
    request.outcome !== undefined ||
    request.zone !== undefined ||
    Boolean(request.name)
  const clearFilters = () => {
    setName('')
    patch({
      [DNS_PARAMS.queryNode]: undefined,
      [DNS_PARAMS.queryOutcome]: undefined,
      [DNS_PARAMS.queryZone]: undefined,
      [DNS_PARAMS.queryPage]: undefined,
    })
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
        <Input
          aria-label="Name contains"
          placeholder="Name contains…"
          className="w-full sm:w-[240px]"
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <Select
          value={request.node_id === undefined ? ALL : String(request.node_id)}
          onValueChange={(value) =>
            update({
              [DNS_PARAMS.queryNode]: value === ALL ? undefined : value,
            })
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
          value={request.outcome ?? ALL}
          onValueChange={(value) =>
            update({
              [DNS_PARAMS.queryOutcome]: value === ALL ? undefined : value,
            })
          }
        >
          <SelectTrigger className="w-full sm:w-[180px]" aria-label="Result">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All results</SelectItem>
            {DNS_OUTCOMES.map((outcome) => (
              <SelectItem key={outcome} value={outcome}>
                {outcomeLabel(outcome)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          value={request.zone ?? ALL}
          onValueChange={(value) =>
            update({
              [DNS_PARAMS.queryZone]: value === ALL ? undefined : value,
            })
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
          title={
            filtered
              ? 'No queries match these filters'
              : enabled
                ? 'No queries yet'
                : 'Cluster DNS is off'
          }
          description={
            filtered
              ? undefined
              : enabled
                ? `Queries for *.temps.local names, and failed forwarded queries, appear here and are kept for ${query.data?.retention_hours ?? 24} hours.`
                : 'Turn on cluster DNS in Overview, and the queries apps make for *.temps.local names appear here.'
          }
          action={
            filtered ? (
              <Button size="sm" variant="outline" onClick={clearFilters}>
                Clear filters
              </Button>
            ) : enabled ? undefined : (
              <Button asChild size="sm" variant="outline">
                <Link to={withParams(search, { tab: undefined })}>
                  Go to Overview
                </Link>
              </Button>
            )
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
            page,
            pageSize,
            total,
            totalPages: Math.max(1, Math.ceil(total / pageSize)),
            onPageChange: (next) =>
              patch({ [DNS_PARAMS.queryPage]: next === 1 ? undefined : next }),
            onPageSizeChange: (next) =>
              update({
                [DNS_PARAMS.queryPageSize]: next === 20 ? undefined : next,
              }),
            pageSizeOptions: DNS_PAGE_SIZES,
            ariaLabel: 'DNS query log pages',
          }}
        />
      )}
    </div>
  )
}
