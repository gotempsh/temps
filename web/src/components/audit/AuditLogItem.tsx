import { AuditLogIpInfo, AuditLogUserInfo } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { TableCell, TableRow } from '@/components/ui/table'
import { cn } from '@/lib/utils'
import { format } from 'date-fns'
import {
  Bell,
  Box,
  ChevronDown,
  ChevronRight,
  Database,
  FileCode,
  FolderKanban,
  GitBranch,
  Globe,
  HardDrive,
  KeyRound,
  LogIn,
  Mail,
  Plug,
  Rocket,
  Server,
  Settings,
  Shield,
  Terminal,
  UserCog,
  Wand2,
  Webhook,
  Workflow,
} from 'lucide-react'
import { ReactNode, useState } from 'react'
import { Link } from 'react-router'

interface AuditLogItemProps {
  id: number
  operation_type: string
  audit_date: number
  user?: AuditLogUserInfo
  ip_address?: AuditLogIpInfo
  data?: Record<string, unknown>
}

type Category =
  | 'auth'
  | 'user'
  | 'mfa'
  | 'project'
  | 'deployment'
  | 'container'
  | 'workspace'
  | 'service'
  | 'backup'
  | 'pipeline'
  | 'skill'
  | 'mcp'
  | 'secret'
  | 'agent'
  | 'domain'
  | 'email'
  | 'webhook'
  | 'notification'
  | 'storage'
  | 'platform'
  | 'other'

// Titlecase an UNKNOWN_OP_TYPE into "Unknown Op Type"
function humanize(op: string): string {
  return op
    .toLowerCase()
    .split('_')
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
}

function truncateDisplay(value: string, maxLength = 80): string {
  return value.length > maxLength ? `${value.slice(0, maxLength - 1)}…` : value
}

function categorize(op: string): Category {
  if (
    op.startsWith('LOGIN_') ||
    op.startsWith('AUTH_') ||
    op === 'USER_LOGOUT' ||
    op === 'PASSWORD_RESET' ||
    op === 'EMAIL_VERIFIED'
  )
    return 'auth'
  if (op.startsWith('USER_') || op.startsWith('ROLE_')) return 'user'
  if (op.startsWith('MFA_')) return 'mfa'
  if (
    op.startsWith('DEPLOYMENT_') ||
    op.startsWith('DEPLOY_FROM_') ||
    op.startsWith('STATIC_BUNDLE_') ||
    op.startsWith('EXTERNAL_IMAGE_')
  )
    return 'deployment'
  if (op === 'CONTAINER_ACTION') return 'container'
  if (op.startsWith('WORKSPACE_')) return 'workspace'
  if (
    op.startsWith('PROJECT_') ||
    op.startsWith('ENVIRONMENT_') ||
    op === 'DEPLOYMENT_CONFIG_UPDATED'
  )
    return 'project'
  if (op.startsWith('EXTERNAL_SERVICE_')) return 'service'
  if (
    op.startsWith('S3_SOURCE_') ||
    op.startsWith('BACKUP_') ||
    op === 'BACKUP_RUN'
  )
    return 'backup'
  if (op.startsWith('PIPELINE_')) return 'pipeline'
  if (op.startsWith('SKILL_')) return 'skill'
  if (op.startsWith('MCP_')) return 'mcp'
  if (op.startsWith('SECRET_')) return 'secret'
  if (op.startsWith('AGENT_') || op.startsWith('AUTOFIXER_')) return 'agent'
  if (op.startsWith('DOMAIN_') || op === 'DNS_CHALLENGE_SETUP') return 'domain'
  if (op.startsWith('EMAIL_')) return 'email'
  if (op.startsWith('WEBHOOK_')) return 'webhook'
  if (op.startsWith('NOTIFICATION_') || op === 'WEEKLY_DIGEST_TRIGGERED')
    return 'notification'
  if (op.startsWith('BLOB_SERVICE_') || op.startsWith('KV_SERVICE_'))
    return 'storage'
  if (
    op === 'SETTINGS_UPDATED' ||
    op === 'JOIN_TOKEN_GENERATED' ||
    op === 'JOIN_TOKEN_REVOKED' ||
    op === 'LOGS_PURGED'
  )
    return 'platform'
  return 'other'
}

const CATEGORY_META: Record<
  Category,
  { label: string; icon: typeof LogIn; tone: string }
> = {
  auth: {
    label: 'Auth',
    icon: LogIn,
    tone: 'bg-blue-500/10 text-blue-600 dark:text-blue-400',
  },
  user: {
    label: 'User',
    icon: UserCog,
    tone: 'bg-purple-500/10 text-purple-600 dark:text-purple-400',
  },
  mfa: {
    label: 'MFA',
    icon: Shield,
    tone: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400',
  },
  project: {
    label: 'Project',
    icon: FolderKanban,
    tone: 'bg-indigo-500/10 text-indigo-600 dark:text-indigo-400',
  },
  service: {
    label: 'Service',
    icon: Plug,
    tone: 'bg-cyan-500/10 text-cyan-600 dark:text-cyan-400',
  },
  backup: {
    label: 'Backup',
    icon: HardDrive,
    tone: 'bg-amber-500/10 text-amber-600 dark:text-amber-400',
  },
  pipeline: {
    label: 'Pipeline',
    icon: GitBranch,
    tone: 'bg-sky-500/10 text-sky-600 dark:text-sky-400',
  },
  skill: {
    label: 'Skill',
    icon: Wand2,
    tone: 'bg-fuchsia-500/10 text-fuchsia-600 dark:text-fuchsia-400',
  },
  mcp: {
    label: 'MCP',
    icon: Workflow,
    tone: 'bg-teal-500/10 text-teal-600 dark:text-teal-400',
  },
  secret: {
    label: 'Secret',
    icon: KeyRound,
    tone: 'bg-rose-500/10 text-rose-600 dark:text-rose-400',
  },
  deployment: {
    label: 'Deploy',
    icon: Rocket,
    tone: 'bg-violet-500/10 text-violet-600 dark:text-violet-400',
  },
  container: {
    label: 'Container',
    icon: Server,
    tone: 'bg-slate-500/10 text-slate-600 dark:text-slate-400',
  },
  workspace: {
    label: 'Workspace',
    icon: Terminal,
    tone: 'bg-zinc-500/10 text-zinc-600 dark:text-zinc-300',
  },
  agent: {
    label: 'Agent',
    icon: Wand2,
    tone: 'bg-pink-500/10 text-pink-600 dark:text-pink-400',
  },
  domain: {
    label: 'Domain',
    icon: Globe,
    tone: 'bg-lime-500/10 text-lime-600 dark:text-lime-400',
  },
  email: {
    label: 'Email',
    icon: Mail,
    tone: 'bg-orange-500/10 text-orange-600 dark:text-orange-400',
  },
  webhook: {
    label: 'Webhook',
    icon: Webhook,
    tone: 'bg-yellow-500/10 text-yellow-700 dark:text-yellow-400',
  },
  notification: {
    label: 'Notification',
    icon: Bell,
    tone: 'bg-red-500/10 text-red-600 dark:text-red-400',
  },
  storage: {
    label: 'Storage',
    icon: Database,
    tone: 'bg-green-500/10 text-green-600 dark:text-green-400',
  },
  platform: {
    label: 'Platform',
    icon: Settings,
    tone: 'bg-stone-500/10 text-stone-600 dark:text-stone-400',
  },
  other: {
    label: 'Other',
    icon: Box,
    tone: 'bg-muted text-muted-foreground',
  },
}

function get<T>(
  data: Record<string, unknown> | undefined,
  key: string
): T | undefined {
  return data?.[key] as T | undefined
}

function projectLink(slug: string): ReactNode {
  return (
    <Link to={`/projects/${slug}`} className="text-primary hover:underline">
      {slug}
    </Link>
  )
}

function describe(
  op: string,
  data?: Record<string, unknown>,
  user?: AuditLogUserInfo
): ReactNode {
  const slug = get<string>(data, 'slug')
  const name = get<string>(data, 'name')
  const projectSlug = get<string>(data, 'project_slug')
  const scope = get<string>(data, 'scope') // "global" | "project"
  const serviceName = get<string>(data, 'service_name')
  const sourceName = get<string>(data, 'source_name')
  const username = get<string>(data, 'username')
  const role = get<string>(data, 'role')
  const status = get<string>(data, 'status')
  const backupId = get<string | number>(data, 'backup_id')
  const secretName = get<string>(data, 'secret_name')
  const domain = get<string>(data, 'domain')
  const imageRef = get<string>(data, 'image_ref')
  const containerId = get<string>(data, 'container_id')
  const action = get<string>(data, 'action')
  const agentSlug = get<string>(data, 'agent_slug')
  const webhookName = get<string>(data, 'webhook_name')
  const providerName = get<string>(data, 'provider_name')
  const sessionId = get<string | number>(data, 'session_id')
  const attemptedEmail = get<string>(data, 'attempted_email')
  const displayedAttemptedEmail = attemptedEmail
    ? truncateDisplay(attemptedEmail)
    : undefined
  const failureReason = get<string>(data, 'reason')

  switch (op) {
    // Auth
    case 'LOGIN_SUCCESS':
      return 'Logged in successfully'
    case 'LOGIN_FAILURE':
      return `Failed login attempt${displayedAttemptedEmail ? ` for ${displayedAttemptedEmail}` : ''}${failureReason ? ` (${humanize(failureReason).toLowerCase()})` : ''}`
    case 'USER_LOGOUT':
      return 'Logged out'
    case 'AUTH_INITIATED':
      return 'Started an authentication flow'
    case 'AUTH_CALLBACK_SUCCESS':
      return 'Completed authentication callback'
    case 'AUTH_CALLBACK_FAILURE':
      return 'Authentication callback failed'

    // Users
    case 'USER_CREATED':
      return `Created user account${username ? ` for ${username}` : ''}`
    case 'USER_UPDATED':
      return `Updated user account${username ? ` for ${username}` : ''}`
    case 'USER_DELETED':
      return `Deleted user account${username ? ` for ${username}` : ''}`
    case 'USER_RESTORED':
      return `Restored user account${username ? ` for ${username}` : ''}`
    case 'ROLE_ASSIGNED':
      return `Assigned role ${role ?? 'unknown'}${username ? ` to ${username}` : ''}`
    case 'ROLE_REMOVED':
      return `Removed role ${role ?? 'unknown'}${username ? ` from ${username}` : ''}`

    // MFA
    case 'MFA_ENABLED':
      return `${user?.name ?? 'User'} enabled multi-factor authentication`
    case 'MFA_DISABLED':
      return `${user?.name ?? 'User'} disabled multi-factor authentication`
    case 'MFA_VERIFIED':
      return `${user?.name ?? 'User'} verified multi-factor authentication`
    case 'MFA_VERIFICATION_FAILED':
      return 'Rejected an MFA verification attempt'

    // External services
    case 'EXTERNAL_SERVICE_CREATED':
      return `Created external service${serviceName ? ` "${serviceName}"` : ''}`
    case 'EXTERNAL_SERVICE_UPDATED':
      return `Updated external service${serviceName ? ` "${serviceName}"` : ''}`
    case 'EXTERNAL_SERVICE_DELETED':
      return `Deleted external service${serviceName ? ` "${serviceName}"` : ''}`
    case 'EXTERNAL_SERVICE_STATUS_CHANGED':
      return `Changed status of external service${serviceName ? ` "${serviceName}"` : ''} to ${status ?? 'unknown'}`
    case 'EXTERNAL_SERVICE_PROJECT_LINKED':
      return projectSlug ? (
        <>Linked external service to project {projectLink(projectSlug)}</>
      ) : (
        'Linked external service to a project'
      )
    case 'EXTERNAL_SERVICE_PROJECT_UNLINKED':
      return projectSlug ? (
        <>Unlinked external service from project {projectLink(projectSlug)}</>
      ) : (
        'Unlinked external service from a project'
      )

    // Projects
    case 'PROJECT_CREATED':
      return projectSlug ? (
        <>Created project {projectLink(projectSlug)}</>
      ) : (
        'Created a new project'
      )
    case 'PROJECT_UPDATED':
      return projectSlug ? (
        <>Updated project {projectLink(projectSlug)}</>
      ) : (
        'Updated a project'
      )
    case 'PROJECT_DELETED':
      return `Deleted project ${projectSlug ?? 'unknown'}`
    case 'PROJECT_GITHUB_UPDATED':
      return projectSlug ? (
        <>Updated GitHub settings for {projectLink(projectSlug)}</>
      ) : (
        'Updated project GitHub settings'
      )
    case 'PROJECT_SETTINGS_UPDATED':
      return projectSlug ? (
        <>Updated settings for {projectLink(projectSlug)}</>
      ) : (
        'Updated project settings'
      )
    case 'ENVIRONMENT_SETTINGS_UPDATED':
      return projectSlug ? (
        <>Updated environment settings for {projectLink(projectSlug)}</>
      ) : (
        'Updated environment settings'
      )

    // Backups
    case 'S3_SOURCE_CREATED':
      return `Created S3 source${sourceName ? ` "${sourceName}"` : ''}`
    case 'S3_SOURCE_UPDATED':
      return `Updated S3 source${sourceName ? ` "${sourceName}"` : ''}`
    case 'S3_SOURCE_DELETED':
      return `Deleted S3 source${sourceName ? ` "${sourceName}"` : ''}`
    case 'BACKUP_SCHEDULE_STATUS_CHANGED':
      return `Changed backup schedule status to ${status ?? 'unknown'}`
    case 'BACKUP_RUN':
      return `Ran backup${backupId != null ? ` (ID: ${backupId})` : ''}`

    // Pipeline
    case 'PIPELINE_TRIGGERED':
      return projectSlug ? (
        <>Triggered pipeline for {projectLink(projectSlug)}</>
      ) : (
        'Triggered a pipeline'
      )

    // Skills (new)
    case 'SKILL_CREATED':
      return `Created ${scope ?? 'project'} skill${slug ? ` "${slug}"` : ''}`
    case 'SKILL_UPDATED':
      return `Updated ${scope ?? 'project'} skill${slug ? ` "${slug}"` : ''}`
    case 'SKILL_DELETED':
      return `Deleted ${scope ?? 'project'} skill${slug ? ` "${slug}"` : ''}`
    case 'SKILL_UPLOADED':
      return `Uploaded archive for ${scope ?? 'project'} skill${slug ? ` "${slug}"` : ''}`

    // MCP servers (new)
    case 'MCP_CREATED':
      return `Created ${scope ?? 'project'} MCP server${slug ? ` "${slug}"` : ''}`
    case 'MCP_UPDATED':
      return `Updated ${scope ?? 'project'} MCP server${slug ? ` "${slug}"` : ''}`
    case 'MCP_DELETED':
      return `Deleted ${scope ?? 'project'} MCP server${slug ? ` "${slug}"` : ''}`

    // Secrets
    case 'SECRET_UPSERTED':
      return `Saved agent secret${secretName ? ` "${secretName}"` : ''}`
    case 'SECRET_DELETED':
      return `Deleted agent secret${secretName ? ` "${secretName}"` : ''}`

    // Auth extras
    case 'PASSWORD_RESET':
      return 'Reset password'
    case 'EMAIL_VERIFIED':
      return 'Verified email address'

    // Projects / environments extras
    case 'DEPLOYMENT_CONFIG_UPDATED':
      return projectSlug ? (
        <>Updated deployment config for {projectLink(projectSlug)}</>
      ) : (
        'Updated deployment config'
      )
    case 'ENVIRONMENT_DELETED':
      return 'Deleted an environment'
    case 'ENVIRONMENT_SLEEP_STATE_CHANGED':
      return `Changed environment sleep state${status ? ` to ${status}` : ''}`

    // Deployments
    case 'DEPLOYMENT_ROLLBACK':
      return 'Rolled back deployment'
    case 'DEPLOYMENT_PAUSED':
      return 'Paused deployment'
    case 'DEPLOYMENT_RESUMED':
      return 'Resumed deployment'
    case 'DEPLOYMENT_CANCELLED':
      return 'Cancelled deployment'
    case 'DEPLOYMENT_TEARDOWN':
      return 'Tore down deployment'
    case 'DEPLOYMENT_PROMOTED':
      return 'Promoted deployment to environment'
    case 'ENVIRONMENT_TEARDOWN':
      return 'Tore down environment'
    case 'DEPLOYMENT_OPERATION_EXECUTED':
      return `Executed deployment operation${action ? ` (${action})` : ''}`
    case 'DEPLOY_FROM_IMAGE':
      return `Deployed from image${imageRef ? ` "${imageRef}"` : ''}`
    case 'DEPLOY_FROM_STATIC':
      return 'Deployed from static bundle'
    case 'DEPLOY_FROM_IMAGE_UPLOAD':
      return 'Deployed from uploaded image'
    case 'STATIC_BUNDLE_UPLOADED':
      return 'Uploaded static bundle'
    case 'STATIC_BUNDLE_DELETED':
      return 'Deleted static bundle'
    case 'EXTERNAL_IMAGE_REGISTERED':
      return `Registered external image${imageRef ? ` "${imageRef}"` : ''}`
    case 'EXTERNAL_IMAGE_PUSHED':
      return `Pushed external image${imageRef ? ` "${imageRef}"` : ''}`
    case 'EXTERNAL_IMAGE_DELETED':
      return 'Deleted external image'

    // Containers
    case 'CONTAINER_ACTION': {
      const verb = action ?? 'action'
      const tail = containerId
        ? ` on container ${containerId.slice(0, 12)}`
        : ''
      return `Performed ${verb}${tail}`
    }

    // Workspaces
    case 'WORKSPACE_TERMINAL_ATTACHED':
      return `Attached to workspace terminal${sessionId != null ? ` (session ${sessionId})` : ''}`
    case 'WORKSPACE_TERMINAL_DETACHED':
      return `Detached from workspace terminal${sessionId != null ? ` (session ${sessionId})` : ''}`

    // Agents & Autofixer
    case 'AGENT_CREATED':
      return `Created agent${agentSlug ? ` "${agentSlug}"` : name ? ` "${name}"` : ''}`
    case 'AGENT_UPDATED':
      return `Updated agent${agentSlug ? ` "${agentSlug}"` : name ? ` "${name}"` : ''}`
    case 'AGENT_DELETED':
      return `Deleted agent${agentSlug ? ` "${agentSlug}"` : name ? ` "${name}"` : ''}`
    case 'AGENT_RUN_TRIGGERED':
      return `Triggered agent run${agentSlug ? ` for "${agentSlug}"` : ''}`
    case 'AUTOFIXER_ANALYSIS_STARTED':
      return 'Started autofixer analysis'
    case 'AUTOFIXER_FIX_STARTED':
      return 'Started autofixer fix'
    case 'AUTOFIXER_PR_CREATED':
      return 'Autofixer opened a pull request'

    // Domains
    case 'DOMAIN_CREATED':
      return `Created domain${domain ? ` "${domain}"` : ''}`
    case 'DOMAIN_DELETED':
      return `Deleted domain${domain ? ` "${domain}"` : ''}`
    case 'DOMAIN_PROVISIONED':
      return `Provisioned domain${domain ? ` "${domain}"` : ''}`
    case 'DOMAIN_RENEWED':
      return `Renewed domain${domain ? ` "${domain}"` : ''}`
    case 'DOMAIN_ORDER_CREATED':
      return `Created domain order${domain ? ` for "${domain}"` : ''}`
    case 'DOMAIN_ORDER_FINALIZED':
      return `Finalized domain order${domain ? ` for "${domain}"` : ''}`
    case 'DOMAIN_ORDER_CANCELLED':
      return `Cancelled domain order${domain ? ` for "${domain}"` : ''}`
    case 'DNS_CHALLENGE_SETUP':
      return `Set up DNS challenge${domain ? ` for "${domain}"` : ''}`

    // Email
    case 'EMAIL_DOMAIN_CREATED':
      return `Added email domain${domain ? ` "${domain}"` : ''}`
    case 'EMAIL_DOMAIN_VERIFIED':
      return `Verified email domain${domain ? ` "${domain}"` : ''}`
    case 'EMAIL_DOMAIN_DELETED':
      return `Removed email domain${domain ? ` "${domain}"` : ''}`
    case 'EMAIL_PROVIDER_CREATED':
      return `Added email provider${providerName ? ` "${providerName}"` : ''}`
    case 'EMAIL_PROVIDER_TESTED':
      return `Tested email provider${providerName ? ` "${providerName}"` : ''}`
    case 'EMAIL_PROVIDER_DELETED':
      return `Removed email provider${providerName ? ` "${providerName}"` : ''}`
    case 'EMAIL_SENT':
      return 'Sent an email'

    // Webhooks
    case 'WEBHOOK_CREATED':
      return `Created webhook${webhookName ? ` "${webhookName}"` : ''}`
    case 'WEBHOOK_UPDATED':
      return `Updated webhook${webhookName ? ` "${webhookName}"` : ''}`
    case 'WEBHOOK_DELETED':
      return `Deleted webhook${webhookName ? ` "${webhookName}"` : ''}`
    case 'WEBHOOK_DELIVERY_RETRIED':
      return 'Retried webhook delivery'

    // Notifications
    case 'NOTIFICATION_PROVIDER_CREATED':
      return `Added notification provider${providerName ? ` "${providerName}"` : ''}`
    case 'NOTIFICATION_PROVIDER_UPDATED':
      return `Updated notification provider${providerName ? ` "${providerName}"` : ''}`
    case 'NOTIFICATION_PROVIDER_TESTED':
      return `Tested notification provider${providerName ? ` "${providerName}"` : ''}`
    case 'NOTIFICATION_PROVIDER_DELETED':
      return `Removed notification provider${providerName ? ` "${providerName}"` : ''}`
    case 'NOTIFICATION_PREFERENCES_UPDATED':
      return 'Updated notification preferences'
    case 'NOTIFICATION_PREFERENCES_DELETED':
      return 'Deleted notification preferences'
    case 'WEEKLY_DIGEST_TRIGGERED':
      return 'Triggered weekly digest'

    // Storage
    case 'BLOB_SERVICE_ENABLED':
      return 'Enabled blob storage'
    case 'BLOB_SERVICE_UPDATED':
      return 'Updated blob storage settings'
    case 'BLOB_SERVICE_DISABLED':
      return 'Disabled blob storage'
    case 'KV_SERVICE_ENABLED':
      return 'Enabled KV storage'
    case 'KV_SERVICE_UPDATED':
      return 'Updated KV storage settings'
    case 'KV_SERVICE_DISABLED':
      return 'Disabled KV storage'

    // Platform
    case 'SETTINGS_UPDATED':
      return 'Updated platform settings'
    case 'JOIN_TOKEN_GENERATED':
      return 'Generated join token'
    case 'JOIN_TOKEN_REVOKED':
      return 'Revoked join token'
    case 'LOGS_PURGED':
      return 'Purged logs'

    default: {
      // Graceful fallback: turn UNKNOWN_OP into "Unknown Op"
      const pretty = humanize(op)
      const target = name || slug || serviceName || sourceName
      return target
        ? `${pretty}: ${target}`
        : pretty || 'Performed an operation'
    }
  }
}

export function AuditLogItemRow({
  operation_type,
  audit_date,
  user,
  ip_address,
  data,
}: AuditLogItemProps) {
  const [expanded, setExpanded] = useState(false)
  const category = categorize(operation_type)
  const meta = CATEGORY_META[category]
  const Icon = meta.icon
  const hasData = data && Object.keys(data).length > 0
  const location = ip_address
    ? [ip_address.city, ip_address.country].filter(Boolean).join(', ')
    : ''

  return (
    <>
      <TableRow
        className={cn(hasData && 'cursor-pointer')}
        onClick={() => hasData && setExpanded((e) => !e)}
      >
        <TableCell className="w-8 pr-0">
          {hasData ? (
            expanded ? (
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-4 w-4 text-muted-foreground" />
            )
          ) : null}
        </TableCell>
        <TableCell className="w-[110px]">
          <Badge
            variant="secondary"
            className={cn('gap-1 font-medium', meta.tone)}
          >
            <Icon className="h-3 w-3" />
            {meta.label}
          </Badge>
        </TableCell>
        <TableCell className="min-w-0">
          <div className="font-medium text-sm">
            {describe(operation_type, data, user)}
          </div>
          <div className="text-xs text-muted-foreground font-mono mt-0.5">
            {operation_type}
          </div>
        </TableCell>
        <TableCell className="hidden md:table-cell text-sm">
          {user?.name ?? (
            <span className="text-muted-foreground italic">system</span>
          )}
        </TableCell>
        <TableCell className="hidden lg:table-cell text-sm text-muted-foreground">
          {ip_address ? (
            <div className="flex flex-col">
              <span>{ip_address.ip}</span>
              {location && <span className="text-xs">{location}</span>}
            </div>
          ) : (
            <span className="italic">—</span>
          )}
        </TableCell>
        <TableCell className="text-right text-sm text-muted-foreground whitespace-nowrap">
          {format(new Date(audit_date), 'PP p')}
        </TableCell>
        <TableCell className="w-8 pl-0">
          {hasData && (
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7"
              onClick={(e) => {
                e.stopPropagation()
                setExpanded((x) => !x)
              }}
            >
              {expanded ? (
                <ChevronDown className="h-4 w-4" />
              ) : (
                <ChevronRight className="h-4 w-4" />
              )}
              <span className="sr-only">Toggle details</span>
            </Button>
          )}
        </TableCell>
      </TableRow>
      {expanded && hasData && (
        <TableRow className="bg-muted/30 hover:bg-muted/30">
          <TableCell />
          <TableCell colSpan={6} className="py-3">
            <div className="space-y-1.5 text-sm">
              {Object.entries(data).map(([key, value]) => (
                <div key={key} className="flex gap-3">
                  <span className="w-[160px] shrink-0 font-medium text-muted-foreground">
                    {key}
                  </span>
                  <pre className="flex-1 whitespace-pre-wrap break-all font-mono text-xs">
                    {typeof value === 'object'
                      ? JSON.stringify(value, null, 2)
                      : String(value)}
                  </pre>
                </div>
              ))}
            </div>
          </TableCell>
        </TableRow>
      )}
    </>
  )
}

// Expose a few helpers to the page (for filter grouping)
export { categorize, humanize, CATEGORY_META }
// Back-compat: legacy card-style component is no longer used, but keep the
// named export so nothing else breaks if someone imports it.
export { AuditLogItemRow as AuditLogItem }

// For the icon-only placeholder used by the Skill icon import above
// (kept to satisfy the type of Icon entries in CATEGORY_META)
export const _FileCodeIcon = FileCode
