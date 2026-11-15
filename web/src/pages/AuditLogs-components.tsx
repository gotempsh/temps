// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Button,
  Callout,
  DataTable,
  PageContainer,
  PageHeader,
  PageState,
  TimeRangeFilter,
  resolveTimeRange,
  useUrlState,
  type DataTableColumn,
} from '@temps-sdk/ds'
import type { AuditLogResponse } from '@/api/client'

import {
  listAuditLogsOptions,
  listAuditLogsQueryKey,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AuditLogItemRow } from '@/components/audit/AuditLogItem'
import { RefreshButton } from '@/components/ui/refresh-button'
import { queryKeyPrefix } from '@/lib/refresh'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from '@/components/ui/searchable-select'

import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { usePageTitle } from '@/hooks/usePageTitle'
import { ALL_FILTER, buildOperationOptions } from './AuditLogs-shared'
import { useQuery } from '@tanstack/react-query'
import { ScrollText, X } from 'lucide-react'
import { useEffect, useMemo, useReducer } from 'react'
import { Navigate } from 'react-router'
import { useWindowQuery } from '@/hooks/useWindowQuery'

const ITEMS_PER_PAGE = 20

export function AuditLogs() {
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([{ label: 'Audit logs' }])
  }, [setBreadcrumbs])
  usePageTitle('Audit logs')
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Audit logs"
        description="Activity across the platform — authentication, service changes, skills, MCP servers, and more."
      />
      <AuditLogView />
    </PageContainer>
  )
}

/** Shared audit table; a record view pins its resource scope across all filters. */
export function AuditLogView({
  resourceId: fixedResourceId,
  active = true,
}: {
  resourceId?: string
  active?: boolean
}) {
  const canViewAuditLogs = useCanViewAuditLogs()
  const { get, patch } = useUrlState<
    'range' | 'operation' | 'user' | 'page' | 'resource'
  >()
  const resourceId = fixedResourceId ?? get('resource') ?? undefined
  const defaultRange = fixedResourceId ? 'all' : '24h'
  const range = get('range') ?? defaultRange
  const operation = get('operation') ?? ALL_FILTER
  const userId = Number(get('user'))
  const selectedUserId =
    Number.isSafeInteger(userId) && userId > 0 ? String(userId) : ALL_FILTER
  const requestedPage = Number(get('page'))
  const page =
    Number.isSafeInteger(requestedPage) &&
    requestedPage > 0 &&
    requestedPage <= 1000000
      ? requestedPage
      : 1
  // Refresh moves a preset window ("Last 24 hours") to now.
  const [windowMoves, moveWindow] = useReducer((n: number) => n + 1, 0)
  const window = useMemo(() => {
    void windowMoves
    return resolveTimeRange(range)
  }, [range, windowMoves])

  const {
    data: users,
    isLoading: isLoadingUsers,
    isError: usersFailed,
    refetch: retryUsers,
  } = useQuery({
    ...listUsersOptions({
      query: { include_deleted: false },
    }),
    enabled: canViewAuditLogs && active,
  })

  const { data, isLoading, isError, refetch, isFetching } = useWindowQuery({
    ...listAuditLogsOptions({
      query: {
        resource_id: resourceId,
        limit: ITEMS_PER_PAGE,
        offset: (page - 1) * ITEMS_PER_PAGE,
        from: range === 'all' ? undefined : window.from,
        to: range === 'all' ? undefined : window.to,
        operation_type: operation !== ALL_FILTER ? operation : undefined,
        user_id:
          selectedUserId !== ALL_FILTER ? Number(selectedUserId) : undefined,
      },
    }),
    enabled: canViewAuditLogs && active,
  })

  const hasMore = data?.length === ITEMS_PER_PAGE
  const showEmptyState = !isLoading && !isError && data?.length === 0
  const hasFilters =
    range !== defaultRange ||
    operation !== ALL_FILTER ||
    selectedUserId !== ALL_FILTER

  const operationOptions = useMemo(() => buildOperationOptions(), [])

  const userOptions = useMemo<SearchableSelectOption[]>(() => {
    const opts: SearchableSelectOption[] = [
      { value: ALL_FILTER, label: 'All users' },
    ]
    for (const u of users ?? []) {
      opts.push({
        value: String(u.user.id),
        label: u.user.name,
        keywords: u.user.email ?? '',
      })
    }
    return opts
  }, [users])

  const resetFilters = () =>
    patch({ range: null, operation: null, user: null, page: null })
  const columns: DataTableColumn<AuditLogResponse>[] = [
    {
      key: 'expand',
      header: <span className="sr-only">Expand</span>,
      className: 'w-8',
      render: () => null,
    },
    { key: 'type', header: 'Type', className: 'w-28', render: () => null },
    { key: 'operation', header: 'Operation', render: () => null },
    {
      key: 'actor',
      header: 'Actor',
      className: 'hidden md:table-cell',
      render: () => null,
    },
    {
      key: 'origin',
      header: 'Origin',
      className: 'hidden lg:table-cell',
      render: () => null,
    },
    {
      key: 'when',
      header: 'When',
      className: 'text-right',
      render: () => null,
    },
    {
      key: 'details',
      header: <span className="sr-only">Details</span>,
      className: 'w-8',
      render: () => null,
    },
  ]

  // Direct navigation guard: only the administration roles may read audit
  // logs, so redirect anyone else away instead of surfacing 403s.
  if (!canViewAuditLogs) {
    return <Navigate to="/projects" replace />
  }

  return (
    <div className="space-y-4">
      <div
        className="flex flex-wrap items-center gap-2"
        role="group"
        aria-label="Audit log filters"
      >
        <SearchableSelect
          label="Operation type"
          value={operation}
          onValueChange={(operation) => patch({ operation, page: null })}
          options={operationOptions}
          title="Filter by operation type"
          placeholder="Filter by type"
          searchPlaceholder="Search types..."
          emptyText="No matching types."
          className="w-full sm:w-56"
        />
        <SearchableSelect
          label="User"
          value={selectedUserId}
          onValueChange={(user) => patch({ user, page: null })}
          options={userOptions}
          title="Filter by user"
          placeholder="Filter by user"
          searchPlaceholder="Search users..."
          emptyText="No matching users."
          disabled={isLoadingUsers || usersFailed}
          className="w-full sm:w-56"
        />
        {range === 'all' ? (
          <Button
            variant="outline"
            onClick={() => patch({ range: '24h', page: null })}
          >
            Choose time range
          </Button>
        ) : (
          <TimeRangeFilter
            value={range}
            onChange={(range) => patch({ range, page: null })}
            maxRangeDays={3650}
          />
        )}
        <Button
          variant={range === 'all' ? 'secondary' : 'ghost'}
          size="sm"
          aria-pressed={range === 'all'}
          onClick={() => patch({ range: 'all', page: null })}
        >
          All time
        </Button>
        {hasFilters && (
          <Button variant="ghost" size="sm" onClick={resetFilters}>
            <X className="size-4" /> Reset filters
          </Button>
        )}
        <RefreshButton
          className="ml-auto"
          queryKeys={[queryKeyPrefix(listAuditLogsQueryKey)]}
          onRefresh={moveWindow}
        />
      </div>
      {resourceId && !fixedResourceId && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="break-all">Resource: {resourceId}</span>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => patch({ resource: null, page: null })}
          >
            Show all resources
          </Button>
        </div>
      )}
      <p className="text-sm text-muted-foreground">
        Times use your browser’s time zone. Results are shown {ITEMS_PER_PAGE}{' '}
        at a time.
      </p>
      {usersFailed && (
        <Callout tone="warning" title="User filter unavailable">
          Audit records are still available.{' '}
          <Button variant="link" size="sm" onClick={() => void retryUsers()}>
            Retry users
          </Button>
        </Callout>
      )}
      {isError && data !== undefined && (
        <Callout tone="error" title="Could not refresh audit logs">
          Showing the last loaded records.{' '}
          <Button variant="link" size="sm" onClick={() => void refetch()}>
            Retry
          </Button>
        </Callout>
      )}
      {isError && data === undefined ? (
        <PageState
          variant="failed"
          size="compact"
          icon={ScrollText}
          title="Could not load audit logs"
          description="The request failed. Retry with your current filters."
          action={<Button onClick={() => void refetch()}>Retry</Button>}
        />
      ) : showEmptyState ? (
        <PageState
          variant="empty"
          size="compact"
          icon={ScrollText}
          title="No audit logs in this selection"
          description={
            page > 1
              ? 'There are no more records on this page.'
              : 'Choose a wider time range or reset the filters.'
          }
          action={
            <Button
              variant="outline"
              onClick={
                page > 1 ? () => patch({ page: page - 1 }) : resetFilters
              }
            >
              {page > 1 ? 'Previous page' : 'Reset filters'}
            </Button>
          }
        />
      ) : (
        <DataTable
          aria-label="Audit logs"
          columns={columns}
          rows={data ?? []}
          rowKey={(log) => log.id}
          isLoading={isLoading}
          renderRow={(log) => (
            <AuditLogItemRow
              id={log.id}
              operation_type={log.operation_type}
              audit_date={log.audit_date}
              user={log.user ?? undefined}
              ip_address={log.ip_address ?? undefined}
              data={log.data as Record<string, unknown> | undefined}
            />
          )}
        />
      )}
      {data !== undefined && !showEmptyState && (
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="text-sm text-muted-foreground">
            Page {page} · {data.length} result{data.length === 1 ? '' : 's'} on
            this page
          </p>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => patch({ page: Math.max(1, page - 1) })}
              disabled={page === 1 || isFetching}
            >
              Previous
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => patch({ page: page + 1 })}
              disabled={!hasMore || isFetching || isError}
            >
              Next
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}
