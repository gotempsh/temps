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
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AuditLogItemRow } from '@/components/audit/AuditLogItem'
import {
  SearchableSelect,
  type SearchableSelectOption,
} from '@/components/ui/searchable-select'

import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { usePageTitle } from '@/hooks/usePageTitle'
import { PERMISSION_DENIED_FILTER } from '@/lib/audit-operation-filters'
import { useQuery } from '@tanstack/react-query'
import { ScrollText, X } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { Navigate } from 'react-router'

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
      PERMISSION_DENIED_FILTER,
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
      { value: 'MFA_VERIFICATION_FAILED', label: 'MFA Verification Failed' },
    ],
  },
  {
    label: 'Projects & Environments',
    operations: [
      { value: 'PROJECT_CREATED', label: 'Project Created' },
      { value: 'PROJECT_UPDATED', label: 'Project Updated' },
      { value: 'PROJECT_DELETED', label: 'Project Deleted' },
      { value: 'PROJECT_SETTINGS_UPDATED', label: 'Project Settings Updated' },
      {
        value: 'DEPLOYMENT_CONFIG_UPDATED',
        label: 'Deployment Config Updated',
      },
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
    operations: [{ value: 'CONTAINER_ACTION', label: 'Container Action' }],
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

export function buildOperationOptions(): SearchableSelectOption[] {
  const options: SearchableSelectOption[] = [
    { value: ALL_FILTER, label: 'All types' },
  ]
  for (const group of OPERATION_GROUPS) {
    for (const operation of group.operations) {
      options.push({
        value: operation.value,
        label: operation.label,
        group: group.label,
        keywords: operation.value,
      })
    }
  }
  return options
}

const ALL_FILTER = '__all__'

export function AuditLogs() {
  const canViewAuditLogs = useCanViewAuditLogs()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { get, patch } = useUrlState<'range' | 'operation' | 'user' | 'page'>()
  const range = get('range') ?? '24h'
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
  const window = useMemo(() => resolveTimeRange(range), [range])

  useEffect(() => {
    setBreadcrumbs([{ label: 'Audit Logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Audit Logs')

  const {
    data: users,
    isLoading: isLoadingUsers,
    isError: usersFailed,
    refetch: retryUsers,
  } = useQuery({
    ...listUsersOptions({
      query: { include_deleted: false },
    }),
    enabled: canViewAuditLogs,
  })

  const { data, isLoading, isError, refetch, isFetching } = useQuery({
    ...listAuditLogsOptions({
      query: {
        limit: ITEMS_PER_PAGE,
        offset: (page - 1) * ITEMS_PER_PAGE,
        from: range === 'all' ? undefined : window.from,
        to: range === 'all' ? undefined : window.to,
        operation_type: operation !== ALL_FILTER ? operation : undefined,
        user_id:
          selectedUserId !== ALL_FILTER ? Number(selectedUserId) : undefined,
      },
    }),
    enabled: canViewAuditLogs,
  })

  const hasMore = data?.length === ITEMS_PER_PAGE
  const showEmptyState = !isLoading && !isError && data?.length === 0
  const hasFilters =
    range !== '24h' || operation !== ALL_FILTER || selectedUserId !== ALL_FILTER

  const operationOptions = useMemo(buildOperationOptions, [])

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
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Audit Logs"
        description="Activity across the platform — authentication, project changes, skills, MCP servers, and more."
      />

      <div
        className="flex flex-wrap items-center gap-2"
        role="group"
        aria-label="Audit log filters"
      >
        <SearchableSelect
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
      </div>
      <p className="text-sm text-muted-foreground">
        Times use your browser’s time zone. Filters apply to all audit records;
        results are shown {ITEMS_PER_PAGE} at a time.
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
    </PageContainer>
  )
}
