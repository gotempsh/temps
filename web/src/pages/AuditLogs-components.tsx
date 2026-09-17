// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import {
  listAuditLogsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AuditLogItemRow } from '@/components/audit/AuditLogItem'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import { EmptyState } from '@/components/ui/empty-state'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from '@/components/ui/searchable-select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ScrollText, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { DateRange } from 'react-day-picker'
import { Navigate } from 'react-router'
import { buildOperationOptions, ALL_FILTER } from './AuditLogs-shared'

const ITEMS_PER_PAGE = 20

export function AuditLogs() {
  const canViewAuditLogs = useCanViewAuditLogs()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [dateRange, setDateRange] = useState<DateRange | undefined>()
  const [operation, setOperation] = useState<string>(ALL_FILTER)
  const [page, setPage] = useState(1)
  const [selectedUserId, setSelectedUserId] = useState<string>(ALL_FILTER)

  useEffect(() => {
    setBreadcrumbs([{ label: 'Audit Logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Audit Logs')

  const { data: users, isLoading: isLoadingUsers } = useQuery({
    ...listUsersOptions({
      query: { include_deleted: false },
    }),
    enabled: canViewAuditLogs,
  })

  const { data, isLoading } = useQuery({
    ...listAuditLogsOptions({
      query: {
        limit: ITEMS_PER_PAGE,
        offset: (page - 1) * ITEMS_PER_PAGE,
        from: dateRange?.from ? dateRange.from.toISOString() : undefined,
        to: dateRange?.to ? dateRange.to.toISOString() : undefined,
        operation_type: operation !== ALL_FILTER ? operation : undefined,
        user_id:
          selectedUserId !== ALL_FILTER ? Number(selectedUserId) : undefined,
      },
    }),
    enabled: canViewAuditLogs,
  })

  const hasMore = useMemo(() => data?.length === ITEMS_PER_PAGE, [data])
  const showEmptyState = useMemo(
    () => !isLoading && (!data || data.length === 0),
    [isLoading, data]
  )
  const hasFilters =
    !!dateRange || operation !== ALL_FILTER || selectedUserId !== ALL_FILTER

  const operationOptions = buildOperationOptions()

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

  const resetFilters = () => {
    setDateRange(undefined)
    setOperation(ALL_FILTER)
    setSelectedUserId(ALL_FILTER)
    setPage(1)
  }

  // Direct navigation guard: only the administration roles may read audit
  // logs, so redirect anyone else away instead of surfacing 403s.
  if (!canViewAuditLogs) {
    return <Navigate to="/projects" replace />
  }

  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Audit Logs"
        description="Activity across the platform — authentication, project changes, skills, MCP servers, and more."
      />

      {/* Filter bar */}
      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
            <DateRangePicker
              date={dateRange}
              onDateChange={setDateRange}
              className="w-full sm:w-[260px]"
            />
            <SearchableSelect
              value={operation}
              onValueChange={setOperation}
              options={operationOptions}
              placeholder="Filter by type"
              searchPlaceholder="Search types..."
              emptyText="No matching types."
              className="w-full sm:w-[220px]"
            />

            <SearchableSelect
              value={selectedUserId}
              onValueChange={setSelectedUserId}
              options={userOptions}
              placeholder="Filter by user"
              searchPlaceholder="Search users..."
              emptyText="No matching users."
              disabled={isLoadingUsers}
              className="w-full sm:w-[220px]"
            />

            {hasFilters && (
              <Button
                variant="ghost"
                size="sm"
                onClick={resetFilters}
                className="ml-auto"
              >
                <X className="h-4 w-4 mr-1" />
                Clear
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      {/* Table */}
      <Card>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8" />
                <TableHead className="w-[110px]">Type</TableHead>
                <TableHead>Operation</TableHead>
                <TableHead className="hidden md:table-cell">Actor</TableHead>
                <TableHead className="hidden lg:table-cell">Origin</TableHead>
                <TableHead className="text-right">When</TableHead>
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {isLoading ? (
                Array.from({ length: 6 }).map((_, i) => (
                  <TableRow key={i}>
                    <TableCell />
                    <TableCell>
                      <Skeleton className="h-5 w-16" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="h-4 w-64" />
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <Skeleton className="h-4 w-24" />
                    </TableCell>
                    <TableCell className="hidden lg:table-cell">
                      <Skeleton className="h-4 w-32" />
                    </TableCell>
                    <TableCell className="text-right">
                      <Skeleton className="h-4 w-32 ml-auto" />
                    </TableCell>
                    <TableCell />
                  </TableRow>
                ))
              ) : showEmptyState ? (
                <TableRow className="hover:bg-transparent">
                  <TableCell colSpan={7} className="p-0">
                    <EmptyState
                      icon={ScrollText}
                      title="No audit logs found"
                      description={
                        hasFilters
                          ? 'Try adjusting your filters to see more results.'
                          : 'Audit logs will appear here when there is activity.'
                      }
                    />
                  </TableCell>
                </TableRow>
              ) : (
                data?.map((log) => (
                  <AuditLogItemRow
                    key={log.id}
                    id={log.id}
                    operation_type={log.operation_type}
                    audit_date={log.audit_date}
                    user={log.user ?? undefined}
                    ip_address={log.ip_address ?? undefined}
                    data={log.data as Record<string, unknown> | undefined}
                  />
                ))
              )}
            </TableBody>
          </Table>
        </div>
      </Card>

      {/* Pagination */}
      {!showEmptyState && (
        <div className="flex items-center justify-between">
          <p className="text-sm text-muted-foreground">
            Page {page}
            {data && ` · ${data.length} result${data.length === 1 ? '' : 's'}`}
          </p>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page === 1 || isLoading}
            >
              Previous
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => p + 1)}
              disabled={!hasMore || isLoading}
            >
              Next
            </Button>
          </div>
        </div>
      )}
    </PageContainer>
  )
}
