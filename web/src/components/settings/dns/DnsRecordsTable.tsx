// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { keepPreviousData, useQuery } from '@tanstack/react-query'
import {
  DataTable,
  PageState,
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
import { formatDistanceToNow } from 'date-fns'
import { BookOpen, Search } from 'lucide-react'
import { Link } from 'react-router'
import { CopyButton } from '@/components/ui/copy-button'
import { ReadFailure } from '@/components/ui/read-failure'
import { useDebounce } from '@/hooks/useDebounce'
import {
  DNS_PAGE_SIZES,
  DNS_PARAMS,
  DNS_RECORD_OWNER_KINDS,
  dnsKeys,
  getDnsRecords,
  parseIntParam,
  parsePageSize,
  recordOwner,
  recordTarget,
  recordTestParams,
  withParams,
  type DnsParam,
  type DnsRecordEntry,
  type DnsRecordsParams,
  type DnsResolverEntry,
} from '@/lib/dns-observability'
import { isForbiddenRead } from '@/lib/read-failure'

const ALL = 'all'

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

function recordColumns(
  testHref: (record: DnsRecordEntry) => string
): DataTableColumn<DnsRecordEntry>[] {
  return [
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
    {
      key: 'test',
      header: <span className="sr-only">Actions</span>,
      className: 'w-0 text-right',
      render: (record) => (
        <Button asChild size="sm" variant="ghost">
          <Link
            to={testHref(record)}
            aria-label={`Test ${record.fqdn}`}
            title={`Test ${record.fqdn}`}
          >
            <Search className="size-4" aria-hidden />
            <span className="max-sm:sr-only">Test</span>
          </Link>
        </Button>
      ),
    },
  ]
}

/**
 * Every name the resolvers answer, what it points to and who owns it, with
 * filters in the URL and a Test link that opens the Test tab prefilled.
 */
export function DnsRecordsTable({
  resolvers,
}: {
  resolvers: DnsResolverEntry[]
}) {
  const urlState = useUrlState<DnsParam>()
  const { get, params: search, patch } = urlState
  const [searchText, setSearchText] = useUrlSearchText(
    DNS_PARAMS.recordSearch,
    { resetKeys: [DNS_PARAMS.recordPage], urlState }
  )
  const debouncedSearch = useDebounce(searchText.trim(), 300)
  const page = Math.max(1, parseIntParam(get(DNS_PARAMS.recordPage)) ?? 1)
  const pageSize = parsePageSize(get(DNS_PARAMS.recordPageSize))
  const request: DnsRecordsParams = {
    page,
    page_size: pageSize,
    search: debouncedSearch || undefined,
    owner_kind: get(DNS_PARAMS.recordOwner) ?? undefined,
    node_id: parseIntParam(get(DNS_PARAMS.recordNode)),
  }
  const update = (values: Partial<Record<DnsParam, string | number>>) =>
    patch({ [DNS_PARAMS.recordPage]: undefined, ...values })

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
  const testHref = (record: DnsRecordEntry) => {
    const test = recordTestParams(record, resolvers)
    return withParams(search, {
      tab: 'test',
      name: test.name,
      type: test.type,
      from: test.from,
      project: undefined,
    })
  }
  const clearFilters = () => {
    setSearchText('')
    patch({
      [DNS_PARAMS.recordOwner]: undefined,
      [DNS_PARAMS.recordNode]: undefined,
      [DNS_PARAMS.recordPage]: undefined,
    })
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
        <Input
          aria-label="Search records"
          placeholder="Search names…"
          className="w-full sm:w-[240px]"
          value={searchText}
          onChange={(event) => setSearchText(event.target.value)}
        />
        <Select
          value={request.owner_kind ?? ALL}
          onValueChange={(value) =>
            update({
              [DNS_PARAMS.recordOwner]: value === ALL ? undefined : value,
            })
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
            value={
              request.node_id === undefined ? ALL : String(request.node_id)
            }
            onValueChange={(value) =>
              update({
                [DNS_PARAMS.recordNode]: value === ALL ? undefined : value,
              })
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
          action={
            filtered ? (
              <Button size="sm" variant="outline" onClick={clearFilters}>
                Clear filters
              </Button>
            ) : undefined
          }
        />
      ) : (
        <DataTable
          aria-label="DNS records"
          columns={recordColumns(testHref)}
          rows={query.data?.items ?? []}
          rowKey={(record) => record.id}
          isLoading={query.isPending}
          pagination={{
            page,
            pageSize,
            total,
            totalPages: Math.max(1, Math.ceil(total / pageSize)),
            onPageChange: (next) =>
              patch({ [DNS_PARAMS.recordPage]: next === 1 ? undefined : next }),
            onPageSizeChange: (next) =>
              update({
                [DNS_PARAMS.recordPageSize]: next === 20 ? undefined : next,
              }),
            pageSizeOptions: DNS_PAGE_SIZES,
            ariaLabel: 'DNS record pages',
          }}
        />
      )}
    </div>
  )
}
