// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type SearchableSelectOption } from '@/components/ui/searchable-select'

import { PERMISSION_DENIED_FILTER } from '@/lib/audit-operation-filters'

export type OperationGroup = {
  label: string
  operations: { value: string; label: string }[]
}

export const OPERATION_GROUPS: OperationGroup[] = [
  {
    label: 'Plugins',
    operations: [
      {
        value: 'EXTERNAL_PLUGIN_HOST_OPERATION_ALLOWED',
        label: 'Plugin operation allowed',
      },
      {
        value: 'EXTERNAL_PLUGIN_HOST_OPERATION_DENIED',
        label: 'Plugin operation denied',
      },
      {
        value: 'EXTERNAL_PLUGIN_HOST_OPERATION_SUCCEEDED',
        label: 'Plugin operation completed',
      },
      {
        value: 'EXTERNAL_PLUGIN_HOST_OPERATION_FAILED',
        label: 'Plugin operation failed',
      },
      {
        value: 'EXTERNAL_PLUGIN_GRANTS_CHANGED',
        label: 'Plugin permissions changed',
      },
      {
        value: 'EXTERNAL_PLUGIN_GRANTS_CHANGE_REQUESTED',
        label: 'Plugin permission change requested',
      },
      {
        value: 'EXTERNAL_PLUGIN_INSTALL_GRANTS_APPROVED',
        label: 'Plugin installation permissions approved',
      },
    ],
  },
  {
    label: 'Authentication',
    operations: [
      { value: 'LOGIN_SUCCESS', label: 'Login success' },
      { value: 'LOGIN_FAILURE', label: 'Login failure' },
      { value: 'OIDC_LOGIN_DENIED', label: 'SSO login denied' },
      { value: 'USER_LOGOUT', label: 'User logout' },
      { value: 'PASSWORD_RESET', label: 'Password reset' },
      { value: 'EMAIL_VERIFIED', label: 'Email verified' },
      PERMISSION_DENIED_FILTER,
    ],
  },
  {
    label: 'Users & roles',
    operations: [
      { value: 'USER_CREATED', label: 'User created' },
      { value: 'USER_UPDATED', label: 'User updated' },
      { value: 'USER_DELETED', label: 'User deleted' },
      { value: 'USER_RESTORED', label: 'User restored' },
      { value: 'ROLE_ASSIGNED', label: 'Role assigned' },
      { value: 'ROLE_REMOVED', label: 'Role removed' },
      { value: 'ADMIN_PASSWORD_RESET', label: 'Password reset by admin' },
    ],
  },
  {
    label: 'MFA',
    operations: [
      { value: 'MFA_SETUP_STARTED', label: 'MFA setup started' },
      { value: 'MFA_ENABLED', label: 'MFA enabled' },
      { value: 'MFA_DISABLED', label: 'MFA disabled' },
      { value: 'MFA_VERIFIED', label: 'MFA verified' },
      { value: 'MFA_VERIFICATION_FAILED', label: 'MFA verification failed' },
    ],
  },
  {
    // Project groups: several services that ship together ("Projects" in
    // the console). A template run or import is logged when it starts and
    // when it is retried; each change it makes is logged as the matching
    // event below, tagged with the run, and its services as Service Created.
    label: 'Projects',
    operations: [
      {
        value: 'TEMPLATE_INSTANTIATION_STARTED',
        label: 'Template or import started',
      },
      {
        value: 'TEMPLATE_INSTANTIATION_RETRIED',
        label: 'Template or import retried',
      },
      { value: 'PROJECT_GROUP_CREATED', label: 'Project created' },
      { value: 'PROJECT_GROUP_UPDATED', label: 'Project updated' },
      { value: 'PROJECT_GROUP_DELETED', label: 'Project deleted' },
      {
        value: 'PROJECT_GROUP_PROJECT_ADDED',
        label: 'Service added to project',
      },
      {
        value: 'PROJECT_GROUP_PROJECT_REMOVED',
        label: 'Service removed from project',
      },
      {
        value: 'PROJECT_GROUP_ENV_VAR_CREATED',
        label: 'Shared variable created',
      },
      {
        value: 'PROJECT_GROUP_ENV_VAR_UPDATED',
        label: 'Shared variable updated',
      },
      {
        value: 'PROJECT_GROUP_ENV_VAR_DELETED',
        label: 'Shared variable deleted',
      },
      {
        value: 'PROJECT_GROUP_REPOSITORY_SET',
        label: 'Project repository set',
      },
      {
        value: 'PROJECT_GROUP_REPOSITORY_CLEARED',
        label: 'Project repository disconnected',
      },
      {
        value: 'PROJECT_GROUP_PROJECT_FOLLOWS_REPOSITORY',
        label: 'Service follows project repository',
      },
      {
        value: 'PROJECT_GROUP_PROJECT_UNFOLLOWS_REPOSITORY',
        label: 'Service stopped following project repository',
      },
      { value: 'PROJECT_GROUP_ROUTE_ADDED', label: 'Project route added' },
      { value: 'PROJECT_GROUP_ROUTE_REMOVED', label: 'Project route removed' },
    ],
  },
  {
    label: 'Services & environments',
    operations: [
      { value: 'PROJECT_CREATED', label: 'Service created' },
      { value: 'PROJECT_UPDATED', label: 'Service updated' },
      { value: 'PROJECT_DELETED', label: 'Service deleted' },
      {
        value: 'PROJECT_SETTINGS_UPDATED',
        label: 'Service settings updated',
      },
      {
        value: 'DEPLOYMENT_CONFIG_UPDATED',
        label: 'Deployment config updated',
      },
      { value: 'ENVIRONMENT_DELETED', label: 'Environment deleted' },
      {
        value: 'ENVIRONMENT_SETTINGS_UPDATED',
        label: 'Environment settings updated',
      },
      {
        value: 'ENVIRONMENT_SLEEP_STATE_CHANGED',
        label: 'Environment sleep state changed',
      },
      { value: 'PIPELINE_TRIGGERED', label: 'Pipeline triggered' },
    ],
  },
  {
    label: 'Deployments',
    operations: [
      { value: 'DEPLOYMENT_ROLLBACK', label: 'Deployment rolled back' },
      { value: 'DEPLOYMENT_PAUSED', label: 'Deployment paused' },
      { value: 'DEPLOYMENT_RESUMED', label: 'Deployment resumed' },
      { value: 'DEPLOYMENT_CANCELLED', label: 'Deployment cancelled' },
      { value: 'DEPLOYMENT_TEARDOWN', label: 'Deployment torn down' },
      { value: 'DEPLOYMENT_PROMOTED', label: 'Deployment promoted' },
      { value: 'ENVIRONMENT_TEARDOWN', label: 'Environment torn down' },
      {
        value: 'DEPLOYMENT_OPERATION_EXECUTED',
        label: 'Deployment operation executed',
      },
      { value: 'DEPLOY_FROM_IMAGE', label: 'Deploy from image' },
      { value: 'DEPLOY_FROM_STATIC', label: 'Deploy from static bundle' },
      {
        value: 'DEPLOY_FROM_IMAGE_UPLOAD',
        label: 'Deploy from uploaded image',
      },
      { value: 'STATIC_BUNDLE_UPLOADED', label: 'Static bundle uploaded' },
      { value: 'STATIC_BUNDLE_DELETED', label: 'Static bundle deleted' },
      {
        value: 'EXTERNAL_IMAGE_REGISTERED',
        label: 'External image registered',
      },
      { value: 'EXTERNAL_IMAGE_PUSHED', label: 'External image pushed' },
      { value: 'EXTERNAL_IMAGE_DELETED', label: 'External image deleted' },
    ],
  },
  {
    label: 'Containers',
    operations: [{ value: 'CONTAINER_ACTION', label: 'Container action' }],
  },
  {
    label: 'Workspaces',
    operations: [
      {
        value: 'WORKSPACE_TERMINAL_ATTACHED',
        label: 'Workspace terminal attached',
      },
      {
        value: 'WORKSPACE_TERMINAL_DETACHED',
        label: 'Workspace terminal detached',
      },
    ],
  },
  {
    label: 'Agents & Autofixer',
    operations: [
      { value: 'AGENT_CREATED', label: 'Agent created' },
      { value: 'AGENT_UPDATED', label: 'Agent updated' },
      { value: 'AGENT_DELETED', label: 'Agent deleted' },
      { value: 'AGENT_RUN_TRIGGERED', label: 'Agent run triggered' },
      {
        value: 'AUTOFIXER_ANALYSIS_STARTED',
        label: 'Autofixer analysis started',
      },
      { value: 'AUTOFIXER_FIX_STARTED', label: 'Autofixer fix started' },
      { value: 'AUTOFIXER_PR_CREATED', label: 'Autofixer PR created' },
    ],
  },
  {
    label: 'Skills',
    operations: [
      { value: 'SKILL_CREATED', label: 'Skill created' },
      { value: 'SKILL_UPDATED', label: 'Skill updated' },
      { value: 'SKILL_DELETED', label: 'Skill deleted' },
      { value: 'SKILL_UPLOADED', label: 'Skill uploaded' },
    ],
  },
  {
    label: 'MCP servers',
    operations: [
      { value: 'MCP_CREATED', label: 'MCP created' },
      { value: 'MCP_UPDATED', label: 'MCP updated' },
      { value: 'MCP_DELETED', label: 'MCP deleted' },
    ],
  },
  {
    label: 'Secrets',
    operations: [
      { value: 'SECRET_UPSERTED', label: 'Secret saved' },
      { value: 'SECRET_DELETED', label: 'Secret deleted' },
    ],
  },
  {
    label: 'External services',
    operations: [
      { value: 'EXTERNAL_SERVICE_CREATED', label: 'External service created' },
      { value: 'EXTERNAL_SERVICE_UPDATED', label: 'External service updated' },
      { value: 'EXTERNAL_SERVICE_DELETED', label: 'External service deleted' },
      {
        value: 'EXTERNAL_SERVICE_STATUS_CHANGED',
        label: 'External service status changed',
      },
      {
        value: 'EXTERNAL_SERVICE_PROJECT_LINKED',
        label: 'External service linked to service',
      },
      {
        value: 'EXTERNAL_SERVICE_PROJECT_UNLINKED',
        label: 'External service unlinked from service',
      },
      {
        value: 'EXTERNAL_SERVICE_BACKUP_RUN',
        label: 'External service backup run',
      },
    ],
  },
  {
    label: 'Backups',
    operations: [
      { value: 'BACKUP_RUN', label: 'Backup run' },
      {
        value: 'BACKUP_SCHEDULE_STATUS_CHANGED',
        label: 'Backup schedule status changed',
      },
    ],
  },
  {
    label: 'Domains',
    operations: [
      { value: 'DOMAIN_CREATED', label: 'Domain created' },
      { value: 'DOMAIN_DELETED', label: 'Domain deleted' },
      { value: 'DOMAIN_PROVISIONED', label: 'Domain provisioned' },
      { value: 'DOMAIN_RENEWED', label: 'Domain renewed' },
      { value: 'DOMAIN_ORDER_CREATED', label: 'Domain order created' },
      { value: 'DOMAIN_ORDER_FINALIZED', label: 'Domain order finalized' },
      { value: 'DOMAIN_ORDER_CANCELLED', label: 'Domain order cancelled' },
      { value: 'DNS_CHALLENGE_SETUP', label: 'DNS challenge setup' },
    ],
  },
  {
    label: 'Email',
    operations: [
      { value: 'EMAIL_DOMAIN_CREATED', label: 'Email domain created' },
      { value: 'EMAIL_DOMAIN_VERIFIED', label: 'Email domain verified' },
      { value: 'EMAIL_DOMAIN_DELETED', label: 'Email domain deleted' },
      { value: 'EMAIL_PROVIDER_CREATED', label: 'Email provider created' },
      { value: 'EMAIL_PROVIDER_TESTED', label: 'Email provider tested' },
      { value: 'EMAIL_PROVIDER_DELETED', label: 'Email provider deleted' },
      { value: 'EMAIL_SENT', label: 'Email sent' },
    ],
  },
  {
    label: 'Webhooks',
    operations: [
      { value: 'WEBHOOK_CREATED', label: 'Webhook created' },
      { value: 'WEBHOOK_UPDATED', label: 'Webhook updated' },
      { value: 'WEBHOOK_DELETED', label: 'Webhook deleted' },
      {
        value: 'WEBHOOK_DELIVERY_RETRIED',
        label: 'Webhook delivery retried',
      },
    ],
  },
  {
    label: 'Notifications',
    operations: [
      {
        value: 'NOTIFICATION_PROVIDER_CREATED',
        label: 'Notification provider created',
      },
      {
        value: 'NOTIFICATION_PROVIDER_UPDATED',
        label: 'Notification provider updated',
      },
      {
        value: 'NOTIFICATION_PROVIDER_TESTED',
        label: 'Notification provider tested',
      },
      {
        value: 'NOTIFICATION_PROVIDER_DELETED',
        label: 'Notification provider deleted',
      },
      {
        value: 'NOTIFICATION_PREFERENCES_UPDATED',
        label: 'Notification preferences updated',
      },
      {
        value: 'NOTIFICATION_PREFERENCES_DELETED',
        label: 'Notification preferences deleted',
      },
      { value: 'WEEKLY_DIGEST_TRIGGERED', label: 'Weekly digest triggered' },
    ],
  },
  {
    label: 'Analytics & visitors',
    operations: [{ value: 'VISITOR_ENRICHED', label: 'Visitor enriched' }],
  },
  {
    label: 'Storage (Blob / KV)',
    operations: [
      { value: 'BLOB_SERVICE_ENABLED', label: 'Blob service enabled' },
      { value: 'BLOB_SERVICE_UPDATED', label: 'Blob service updated' },
      { value: 'BLOB_SERVICE_DISABLED', label: 'Blob service disabled' },
      { value: 'KV_SERVICE_ENABLED', label: 'KV service enabled' },
      { value: 'KV_SERVICE_UPDATED', label: 'KV service updated' },
      { value: 'KV_SERVICE_DISABLED', label: 'KV service disabled' },
    ],
  },
  {
    label: 'Platform',
    operations: [
      { value: 'SETTINGS_UPDATED', label: 'Settings updated' },
      { value: 'JOIN_TOKEN_GENERATED', label: 'Join token generated' },
      { value: 'JOIN_TOKEN_REVOKED', label: 'Join token revoked' },
      { value: 'LOGS_PURGED', label: 'Logs purged' },
    ],
  },
]

// Pure option builder is exported for focused coverage of the audit vocabulary.
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

export const ALL_FILTER = '__all__'
