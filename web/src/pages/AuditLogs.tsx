import {
  listAuditLogsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AuditLogItem } from '@/components/audit/AuditLogItem'
import { Card } from '@/components/ui/card'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { ScrollText } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { DateRange } from 'react-day-picker'

const ITEMS_PER_PAGE = 20

type OperationGroup = {
  label: string
  operations: { value: string; label: string }[]
}

const OPERATION_GROUPS: OperationGroup[] = [
  {
    label: 'Authentication',
    operations: [
      { value: 'LOGIN_SUCCESS', label: 'Login Success' },
      { value: 'LOGIN_FAILURE', label: 'Login Failure' },
      { value: 'AUTH_INITIATED', label: 'Auth Initiated' },
      { value: 'AUTH_CALLBACK_SUCCESS', label: 'Auth Callback Success' },
      { value: 'AUTH_CALLBACK_FAILURE', label: 'Auth Callback Failure' },
      { value: 'USER_LOGOUT', label: 'User Logout' },
    ],
  },
  {
    label: 'Users',
    operations: [
      { value: 'USER_CREATED', label: 'User Created' },
      { value: 'USER_UPDATED', label: 'User Updated' },
      { value: 'USER_DELETED', label: 'User Deleted' },
      { value: 'USER_RESTORED', label: 'User Restored' },
      { value: 'ROLE_ASSIGNED', label: 'Role Assigned' },
      { value: 'ROLE_REMOVED', label: 'Role Removed' },
    ],
  },
  {
    label: 'MFA',
    operations: [
      { value: 'MFA_ENABLED', label: 'MFA Enabled' },
      { value: 'MFA_DISABLED', label: 'MFA Disabled' },
      { value: 'MFA_VERIFIED', label: 'MFA Verified' },
    ],
  },
  {
    label: 'Projects',
    operations: [
      { value: 'PROJECT_CREATED', label: 'Project Created' },
      { value: 'PROJECT_UPDATED', label: 'Project Updated' },
      { value: 'PROJECT_DELETED', label: 'Project Deleted' },
      { value: 'PROJECT_GITHUB_UPDATED', label: 'Project GitHub Updated' },
      { value: 'PROJECT_SETTINGS_UPDATED', label: 'Project Settings Updated' },
      {
        value: 'ENVIRONMENT_SETTINGS_UPDATED',
        label: 'Environment Settings Updated',
      },
      { value: 'PIPELINE_TRIGGERED', label: 'Pipeline Triggered' },
    ],
  },
]

export function AuditLogs() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [dateRange, setDateRange] = useState<DateRange | undefined>()
  const [operation, setOperation] = useState<string | ''>('')
  const [page, setPage] = useState(1)
  const [selectedUserId, setSelectedUserId] = useState<string>('')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Audit Logs' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Audit Logs')

  const { data: users, isLoading: isLoadingUsers } = useQuery(
    listUsersOptions({
      query: {
        include_deleted: false,
      },
    })
  )

  const { data, isLoading } = useQuery(
    listAuditLogsOptions({
      query: {
        limit: ITEMS_PER_PAGE,
        offset: (page - 1) * ITEMS_PER_PAGE,
        from: dateRange?.from
          ? Number(format(dateRange.from, 'yyyyMMdd'))
          : undefined,
        to: dateRange?.to
          ? Number(format(dateRange.to, 'yyyyMMdd'))
          : undefined,
        operation_type: operation || undefined,
        user_id: selectedUserId ? Number(selectedUserId) : undefined,
      },
    })
  )

  const hasMore = useMemo(() => data?.length === ITEMS_PER_PAGE, [data])
  const showEmptyState = useMemo(
    () => !isLoading && (!data || data.length === 0),
    [isLoading, data]
  )

  return (
    <div className="space-y-4">
      <div className="flex flex-col sm:flex-row gap-4">
        <DateRangePicker
          date={dateRange}
          onDateChange={setDateRange}
          className="w-full sm:w-[300px]"
        />
        <Select value={operation} onValueChange={setOperation}>
          <SelectTrigger className="w-full sm:w-[200px]">
            <SelectValue placeholder="Filter by type" />
          </SelectTrigger>
          <SelectContent>
            {OPERATION_GROUPS.map((group) => (
              <SelectGroup key={group.label}>
                <SelectLabel className="text-xs font-medium text-muted-foreground py-1.5">
                  {group.label}
                </SelectLabel>
                {group.operations.map((op) => (
                  <SelectItem key={op.value} value={op.value}>
                    {op.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            ))}
          </SelectContent>
        </Select>

        <Select value={selectedUserId} onValueChange={setSelectedUserId}>
          <SelectTrigger
            className="w-full sm:w-[200px]"
            disabled={isLoadingUsers}
          >
            <SelectValue placeholder="Filter by user" />
          </SelectTrigger>
          <SelectContent>
            {users?.map((user) => (
              <SelectItem key={user.user.id} value={String(user.user.id)}>
                {user.user.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="space-y-4">
        {isLoading ? (
          Array.from({ length: 5 }).map((_, i) => (
            <Card key={i} className="p-4">
              <Skeleton className="h-4 w-full" />
            </Card>
          ))
        ) : showEmptyState ? (
          <Card className="p-12">
            <div className="flex flex-col items-center justify-center text-center space-y-3">
              <div className="bg-muted rounded-full p-3">
                <ScrollText className="h-6 w-6 text-muted-foreground" />
              </div>
              <div className="space-y-1">
                <h3 className="font-medium text-lg">No audit logs found</h3>
                <p className="text-sm text-muted-foreground">
                  {dateRange || operation
                    ? 'Try adjusting your filters to see more results'
                    : 'Audit logs will appear here when there is activity'}
                </p>
              </div>
            </div>
          </Card>
        ) : (
          <div className="space-y-4">
            {data?.map((log) => (
              <AuditLogItem
                key={log.id}
                id={log.id}
                operation_type={log.operation_type}
                audit_date={log.audit_date}
                user={log.user as any}
                ip_address={log.ip_address as any}
                data={log.data as any}
              />
            ))}
          </div>
        )}

        {!showEmptyState && (
          <div className="flex justify-center gap-2">
            <button
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page === 1 || isLoading}
              className="px-4 py-2 text-sm font-medium text-gray-700 bg-white border border-gray-300 rounded-md hover:bg-gray-50 disabled:opacity-50"
            >
              Previous
            </button>
            <button
              onClick={() => setPage((p) => p + 1)}
              disabled={!hasMore || isLoading}
              className="px-4 py-2 text-sm font-medium text-gray-700 bg-white border border-gray-300 rounded-md hover:bg-gray-50 disabled:opacity-50"
            >
              Next
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
