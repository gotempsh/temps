// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AuditLogIpInfo, AuditLogUserInfo } from '@/api/client'
import {
  Bell,
  Box,
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

export interface AuditLogItemProps {
  id: number
  operation_type: string
  audit_date: number
  user?: AuditLogUserInfo
  ip_address?: AuditLogIpInfo
  data?: Record<string, unknown>
}

export type Category =
  | 'plugin'
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
export function humanize(op: string): string {
  return op
    .toLowerCase()
    .split('_')
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
}

export function categorize(op: string): Category {
  if (op.startsWith('EXTERNAL_PLUGIN_')) return 'plugin'
  if (
    op.startsWith('LOGIN_') ||
    op.startsWith('AUTH_') ||
    op.startsWith('OIDC_') ||
    op === 'USER_LOGOUT' ||
    op === 'PASSWORD_RESET' ||
    op === 'EMAIL_VERIFIED' ||
    op === 'PERMISSION_DENIED'
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

export const CATEGORY_META: Record<
  Category,
  { label: string; icon: typeof LogIn; tone: string }
> = {
  plugin: {
    label: 'Plugin',
    icon: Plug,
    tone: 'bg-muted text-muted-foreground',
  },
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

// For the icon-only placeholder used by the Skill icon import above
// (kept to satisfy the type of Icon entries in CATEGORY_META)
export const _FileCodeIcon = FileCode
