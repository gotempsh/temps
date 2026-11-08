// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { DataTable, PageState, type DataTableColumn } from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import { formatDistanceToNow } from 'date-fns'
import { BookOpen } from 'lucide-react'
import { useState } from 'react'
import { CopyButton } from '@/components/ui/copy-button'
import { ReadFailure } from '@/components/ui/read-failure'
import { useDebounce } from '@/hooks/useDebounce'
import {
  DNS_RECORD_OWNER_KINDS,
  dnsKeys,
  getDnsRecords,
  recordOwner,
  recordTarget,
  type DnsRecordEntry,
  type DnsRecordsParams,
  type DnsResolverEntry,
} from '@/lib/dns-observability'
import { isForbiddenRead } from '@/lib/read-failure'

const ALL = 'all'
const PAGE_SIZE_OPTIONS = [20, 50, 100] as const

function NameCell({ record }: { record: DnsRecordEntry }) {
  return (
    <div className="min-w-0 space-y-0.5">
      <div className="flex items-center gap-1">
        <span className="font-mono text-xs [overflow-wrap:anywhere]">
          {record.fqdn}
        </span>
        <CopyButton value={record.fqdn} label="Copy name" />
      </div>
      {/* What the hidden columns say, for a narrow table. */}
      <p className="text-xs text-muted-foreground @2xl:hidden [overflow-wrap:anywhere]">
        {recordOwner(record)}
        {record.node_name ? ` · ${record.node_name}` : ''}
      </p>
    </div>
  )
}

const columns: DataTableColumn<DnsRecordEntry>[] = [
  {
    key: 'name',
    header: 'Name',
    render: (record) => <NameCell record={record} />,
  },
  {
    key: 'target',
    header: 'Target',
    render: (record) => (
      <span className="font-mono text-xs [overflow-wrap:anywhere]">
        <span className="mr-1.5 text-muted-foreground">
          {record.record_type}
        </span>
        {recordTarget(record)}
        <span className="ml-1.5 text-muted-foreground">TTL {record.ttl}</span>
      </span>
    ),
  },
  {
    key: 'owner',
    header: 'Owner',
    className: 'hidden @2xl:table-cell',
    render: (record) => (
      <span className="[overflow-wrap:anywhere]">{recordOwner(record)}</span>
    ),
  },
  {
    key: 'node',
    header: 'Node',
    className: 'hidden @3xl:table-cell',
    render: (record) => record.node_name ?? 'Control plane',
  },
  {
    key: 'updated',
    header: 'Last updated',
    className: 'hidden @4xl:table-cell text-muted-foreground',
    render: (record) =>
      formatDistanceToNow(new Date(record.updated_at), { addSuffix: true }),
  },
]

/** Every name the resolvers answer, what it points to and who owns it. */
export function DnsRecordsTable({
  resolvers,
}: {
  resolvers: DnsResolverEntry[]
}) {
  const [params, setParams] = useState<DnsRecordsParams>({
    page: 1,
    page_size: 20,
  })
  const [search, setSearch] = useState('')
  const debouncedSearch = useDebounce(search.trim(), 300)
  const request: DnsRecordsParams = {
    ...params,
    search: debouncedSearch || undefined,
  }
  const update = (patch: Partial<DnsRecordsParams>) =>
    setParams((current) => ({ ...current, page: 1, ...patch }))

  const query = useQuery({
    queryKey: dnsKeys.records(request),
    queryFn: () => getDnsRecords(request),
    placeholderData: keepPreviousData,
    meta: { errorTitle: 'Could not load the DNS records' },
  })
  const total = query.data?.total ?? 0
  const filtered =
    Boolean(request.search) ||
    request.node_id !== undefined ||
    request.owner_kind !== undefined
  // Records on a worker carry its id; the control plane's carry none.
  const workers = resolvers.filter((resolver) => !resolver.is_control_plane)

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
        <Input
          aria-label="Search records"
          placeholder="Search names…"
          className="w-full sm:w-[240px]"
          value={search}
          onChange={(event) => {
            setSearch(event.target.value)
            setParams((current) => ({ ...current, page: 1 }))
          }}
        />
        <Select
          value={params.owner_kind ?? ALL}
          onValueChange={(value) =>
            update({ owner_kind: value === ALL ? undefined : value })
          }
        >
          <SelectTrigger className="w-full sm:w-[200px]" aria-label="Owner">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All owners</SelectItem>
            {DNS_RECORD_OWNER_KINDS.map((kind) => (
              <SelectItem key={kind.value} value={kind.value}>
                {kind.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {workers.length > 0 ? (
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
              {workers.map((resolver) => (
                <SelectItem
                  key={resolver.node_id}
                  value={String(resolver.node_id)}
                >
                  {resolver.node_name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : null}
      </div>

      {query.isError ? (
        <ReadFailure
          resource="DNS records"
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
          icon={BookOpen}
          title={filtered ? 'No records match these filters' : 'No records yet'}
          description={
            filtered
              ? undefined
              : 'A record appears for each environment, database and node once it is deployed, for example production.shop.temps.local.'
          }
        />
      ) : (
        <DataTable
          aria-label="DNS records"
          columns={columns}
          rows={query.data?.items ?? []}
          rowKey={(record) => record.id}
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
            ariaLabel: 'DNS record pages',
          }}
        />
      )}
    </div>
  )
}
