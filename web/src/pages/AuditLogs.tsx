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
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ScrollText, X } from 'lucide-react'
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
      { value: 'USER_LOGOUT', label: 'User Logout' },
      { value: 'PASSWORD_RESET', label: 'Password Reset' },
      { value: 'EMAIL_VERIFIED', label: 'Email Verified' },
    ],
  },
  {
    label: 'Users & Roles',
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
    label: 'Projects & Environments',
    operations: [
      { value: 'PROJECT_CREATED', label: 'Project Created' },
      { value: 'PROJECT_UPDATED', label: 'Project Updated' },
      { value: 'PROJECT_DELETED', label: 'Project Deleted' },
      { value: 'PROJECT_SETTINGS_UPDATED', label: 'Project Settings Updated' },
      { value: 'DEPLOYMENT_CONFIG_UPDATED', label: 'Deployment Config Updated' },
      { value: 'ENVIRONMENT_DELETED', label: 'Environment Deleted' },
      {
        value: 'ENVIRONMENT_SETTINGS_UPDATED',
        label: 'Environment Settings Updated',
      },
      {
        value: 'ENVIRONMENT_SLEEP_STATE_CHANGED',
        label: 'Environment Sleep State Changed',
      },
      { value: 'PIPELINE_TRIGGERED', label: 'Pipeline Triggered' },
    ],
  },
  {
    label: 'Deployments',
    operations: [
      { value: 'DEPLOYMENT_ROLLBACK', label: 'Deployment Rolled Back' },
      { value: 'DEPLOYMENT_PAUSED', label: 'Deployment Paused' },
      { value: 'DEPLOYMENT_RESUMED', label: 'Deployment Resumed' },
      { value: 'DEPLOYMENT_CANCELLED', label: 'Deployment Cancelled' },
      { value: 'DEPLOYMENT_TEARDOWN', label: 'Deployment Torn Down' },
      { value: 'DEPLOYMENT_PROMOTED', label: 'Deployment Promoted' },
      { value: 'ENVIRONMENT_TEARDOWN', label: 'Environment Torn Down' },
      {
        value: 'DEPLOYMENT_OPERATION_EXECUTED',
        label: 'Deployment Operation Executed',
      },
      { value: 'DEPLOY_FROM_IMAGE', label: 'Deploy From Image' },
      { value: 'DEPLOY_FROM_STATIC', label: 'Deploy From Static Bundle' },
      {
        value: 'DEPLOY_FROM_IMAGE_UPLOAD',
        label: 'Deploy From Uploaded Image',
      },
      { value: 'STATIC_BUNDLE_UPLOADED', label: 'Static Bundle Uploaded' },
      { value: 'STATIC_BUNDLE_DELETED', label: 'Static Bundle Deleted' },
      {
        value: 'EXTERNAL_IMAGE_REGISTERED',
        label: 'External Image Registered',
      },
      { value: 'EXTERNAL_IMAGE_PUSHED', label: 'External Image Pushed' },
      { value: 'EXTERNAL_IMAGE_DELETED', label: 'External Image Deleted' },
    ],
  },
  {
    label: 'Containers',
    operations: [
      { value: 'CONTAINER_ACTION', label: 'Container Action' },
    ],
  },
  {
    label: 'Workspaces',
    operations: [
      {
        value: 'WORKSPACE_TERMINAL_ATTACHED',
        label: 'Workspace Terminal Attached',
      },
      {
        value: 'WORKSPACE_TERMINAL_DETACHED',
        label: 'Workspace Terminal Detached',
      },
    ],
  },
  {
    label: 'Agents & Autofixer',
    operations: [
      { value: 'AGENT_CREATED', label: 'Agent Created' },
      { value: 'AGENT_UPDATED', label: 'Agent Updated' },
      { value: 'AGENT_DELETED', label: 'Agent Deleted' },
      { value: 'AGENT_RUN_TRIGGERED', label: 'Agent Run Triggered' },
      {
        value: 'AUTOFIXER_ANALYSIS_STARTED',
        label: 'Autofixer Analysis Started',
      },
      { value: 'AUTOFIXER_FIX_STARTED', label: 'Autofixer Fix Started' },
      { value: 'AUTOFIXER_PR_CREATED', label: 'Autofixer PR Created' },
    ],
  },
  {
    label: 'Skills',
    operations: [
      { value: 'SKILL_CREATED', label: 'Skill Created' },
      { value: 'SKILL_UPDATED', label: 'Skill Updated' },
      { value: 'SKILL_DELETED', label: 'Skill Deleted' },
      { value: 'SKILL_UPLOADED', label: 'Skill Uploaded' },
    ],
  },
  {
    label: 'MCP Servers',
    operations: [
      { value: 'MCP_CREATED', label: 'MCP Created' },
      { value: 'MCP_UPDATED', label: 'MCP Updated' },
      { value: 'MCP_DELETED', label: 'MCP Deleted' },
    ],
  },
  {
    label: 'Secrets',
    operations: [
      { value: 'SECRET_UPSERTED', label: 'Secret Saved' },
      { value: 'SECRET_DELETED', label: 'Secret Deleted' },
    ],
  },
  {
    label: 'External Services',
    operations: [
      { value: 'EXTERNAL_SERVICE_CREATED', label: 'External Service Created' },
      { value: 'EXTERNAL_SERVICE_UPDATED', label: 'External Service Updated' },
      { value: 'EXTERNAL_SERVICE_DELETED', label: 'External Service Deleted' },
      {
        value: 'EXTERNAL_SERVICE_STATUS_CHANGED',
        label: 'External Service Status Changed',
      },
      {
        value: 'EXTERNAL_SERVICE_PROJECT_LINKED',
        label: 'External Service Linked to Project',
      },
      {
        value: 'EXTERNAL_SERVICE_PROJECT_UNLINKED',
        label: 'External Service Unlinked from Project',
      },
      {
        value: 'EXTERNAL_SERVICE_BACKUP_RUN',
        label: 'External Service Backup Run',
      },
    ],
  },
  {
    label: 'Backups',
    operations: [
      { value: 'BACKUP_RUN', label: 'Backup Run' },
      {
        value: 'BACKUP_SCHEDULE_STATUS_CHANGED',
        label: 'Backup Schedule Status Changed',
      },
    ],
  },
  {
    label: 'Domains',
    operations: [
      { value: 'DOMAIN_CREATED', label: 'Domain Created' },
      { value: 'DOMAIN_DELETED', label: 'Domain Deleted' },
      { value: 'DOMAIN_PROVISIONED', label: 'Domain Provisioned' },
      { value: 'DOMAIN_RENEWED', label: 'Domain Renewed' },
      { value: 'DOMAIN_ORDER_CREATED', label: 'Domain Order Created' },
      { value: 'DOMAIN_ORDER_FINALIZED', label: 'Domain Order Finalized' },
      { value: 'DOMAIN_ORDER_CANCELLED', label: 'Domain Order Cancelled' },
      { value: 'DNS_CHALLENGE_SETUP', label: 'DNS Challenge Setup' },
    ],
  },
  {
    label: 'Email',
    operations: [
      { value: 'EMAIL_DOMAIN_CREATED', label: 'Email Domain Created' },
      { value: 'EMAIL_DOMAIN_VERIFIED', label: 'Email Domain Verified' },
      { value: 'EMAIL_DOMAIN_DELETED', label: 'Email Domain Deleted' },
      { value: 'EMAIL_PROVIDER_CREATED', label: 'Email Provider Created' },
      { value: 'EMAIL_PROVIDER_TESTED', label: 'Email Provider Tested' },
      { value: 'EMAIL_PROVIDER_DELETED', label: 'Email Provider Deleted' },
      { value: 'EMAIL_SENT', label: 'Email Sent' },
    ],
  },
  {
    label: 'Webhooks',
    operations: [
      { value: 'WEBHOOK_CREATED', label: 'Webhook Created' },
      { value: 'WEBHOOK_UPDATED', label: 'Webhook Updated' },
      { value: 'WEBHOOK_DELETED', label: 'Webhook Deleted' },
      {
        value: 'WEBHOOK_DELIVERY_RETRIED',
        label: 'Webhook Delivery Retried',
      },
    ],
  },
  {
    label: 'Notifications',
    operations: [
      {
        value: 'NOTIFICATION_PROVIDER_CREATED',
        label: 'Notification Provider Created',
      },
      {
        value: 'NOTIFICATION_PROVIDER_UPDATED',
        label: 'Notification Provider Updated',
      },
      {
        value: 'NOTIFICATION_PROVIDER_TESTED',
        label: 'Notification Provider Tested',
      },
      {
        value: 'NOTIFICATION_PROVIDER_DELETED',
        label: 'Notification Provider Deleted',
      },
      {
        value: 'NOTIFICATION_PREFERENCES_UPDATED',
        label: 'Notification Preferences Updated',
      },
      {
        value: 'NOTIFICATION_PREFERENCES_DELETED',
        label: 'Notification Preferences Deleted',
      },
      { value: 'WEEKLY_DIGEST_TRIGGERED', label: 'Weekly Digest Triggered' },
    ],
  },
  {
    label: 'Storage (Blob / KV)',
    operations: [
      { value: 'BLOB_SERVICE_ENABLED', label: 'Blob Service Enabled' },
      { value: 'BLOB_SERVICE_UPDATED', label: 'Blob Service Updated' },
      { value: 'BLOB_SERVICE_DISABLED', label: 'Blob Service Disabled' },
      { value: 'KV_SERVICE_ENABLED', label: 'KV Service Enabled' },
      { value: 'KV_SERVICE_UPDATED', label: 'KV Service Updated' },
      { value: 'KV_SERVICE_DISABLED', label: 'KV Service Disabled' },
    ],
  },
  {
    label: 'Platform',
    operations: [
      { value: 'SETTINGS_UPDATED', label: 'Settings Updated' },
      { value: 'JOIN_TOKEN_GENERATED', label: 'Join Token Generated' },
      { value: 'JOIN_TOKEN_REVOKED', label: 'Join Token Revoked' },
      { value: 'LOGS_PURGED', label: 'Logs Purged' },
    ],
  },
]

const ALL_FILTER = '__all__'

export function AuditLogs() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [dateRange, setDateRange] = useState<DateRange | undefined>()
  const [operation, setOperation] = useState<string>(ALL_FILTER)
  const [page, setPage] = useState(1)
  const [selectedUserId, setSelectedUserId] = useState<string>(ALL_FILTER)

  useEffect(() => {
    setBreadcrumbs([{ label: 'Audit Logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Audit Logs')

  const { data: users, isLoading: isLoadingUsers } = useQuery(
    listUsersOptions({
      query: { include_deleted: false },
    })
  )

  const { data, isLoading } = useQuery(
    listAuditLogsOptions({
      query: {
        limit: ITEMS_PER_PAGE,
        offset: (page - 1) * ITEMS_PER_PAGE,
        from: dateRange?.from ? dateRange.from.toISOString() : undefined,
        to: dateRange?.to ? dateRange.to.toISOString() : undefined,
        operation_type: operation !== ALL_FILTER ? operation : undefined,
        user_id:
          selectedUserId !== ALL_FILTER ? Number(selectedUserId) : undefined,
      },
    })
  )

  const hasMore = useMemo(() => data?.length === ITEMS_PER_PAGE, [data])
  const showEmptyState = useMemo(
    () => !isLoading && (!data || data.length === 0),
    [isLoading, data]
  )
  const hasFilters =
    !!dateRange || operation !== ALL_FILTER || selectedUserId !== ALL_FILTER

  const operationOptions = useMemo<SearchableSelectOption[]>(() => {
    const opts: SearchableSelectOption[] = [
      { value: ALL_FILTER, label: 'All types' },
    ]
    for (const group of OPERATION_GROUPS) {
      for (const op of group.operations) {
        opts.push({
          value: op.value,
          label: op.label,
          group: group.label,
          keywords: op.value,
        })
      }
    }
    return opts
  }, [])

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

  return (
    <div className="space-y-4">
      {/* Page header */}
      <div className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Audit Logs</h1>
        <p className="text-sm text-muted-foreground">
          Activity across the platform — authentication, project changes,
          skills, MCP servers, and more.
        </p>
      </div>

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
                    data={
                      log.data as Record<string, unknown> | undefined
                    }
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
    </div>
  )
}
