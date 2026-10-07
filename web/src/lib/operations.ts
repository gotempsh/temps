// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for the header operations tray (`GET /operations`).
 *
 * Kept free of the generated SDK's runtime exports so the logic is testable
 * on its own; only types are imported from the generated client.
 */
import type {
  OperationEntry,
  OperationKind,
  OperationStatus,
} from '@/api/client/types.gen'
import type { QueryClient } from '@tanstack/react-query'
import {
  Archive,
  ArrowUpRight,
  DatabaseBackup,
  RefreshCw,
  Rocket,
  RotateCcw,
  Wand2,
  type LucideIcon,
} from 'lucide-react'

/** The generated query id for `GET /operations` (`listOperationsQueryKey`). */
export const OPERATIONS_QUERY_ID = 'listOperations'

/** Rows shown in the tray. The API's default page size. */
export const OPERATIONS_TRAY_PAGE_SIZE = 20

/** Poll interval while the tray is open. */
export const OPERATIONS_OPEN_POLL_MS = 5_000

/** Poll interval while something is running and the tray is closed. */
export const OPERATIONS_ACTIVE_POLL_MS = 10_000

/**
 * Refresh every operations feed query (any page/filter). Call after starting
 * anything that shows up in the tray so the badge updates immediately rather
 * than on the next poll.
 *
 * Matches on the generated key's `_id`, so it doesn't need the SDK at runtime.
 */
export function invalidateOperations(queryClient: QueryClient): Promise<void> {
  return queryClient.invalidateQueries({
    queryKey: [{ _id: OPERATIONS_QUERY_ID }],
  })
}

/**
 * How often to refetch the feed: fast while the tray is open, slower while
 * something is in flight (server-side or a local entry), never otherwise.
 */
export function operationsPollInterval({
  open,
  runningCount,
  localCount = 0,
}: {
  open: boolean
  runningCount: number
  localCount?: number
}): number | false {
  if (open) return OPERATIONS_OPEN_POLL_MS
  if (runningCount > 0 || localCount > 0) return OPERATIONS_ACTIVE_POLL_MS
  return false
}

const ACTIVE_STATUSES: ReadonlySet<OperationStatus> = new Set([
  'queued',
  'running',
  'waiting',
])

export function isActiveOperationStatus(status: OperationStatus): boolean {
  return ACTIVE_STATUSES.has(status)
}

export const OPERATION_STATUS_LABEL: Record<OperationStatus, string> = {
  queued: 'Queued',
  running: 'Running',
  waiting: 'Needs review',
  succeeded: 'Succeeded',
  failed: 'Failed',
  cancelled: 'Cancelled',
}

export type OperationStatusVariant =
  'secondary' | 'success' | 'warning' | 'destructive' | 'outline'

const OPERATION_STATUS_VARIANT: Record<
  OperationStatus,
  OperationStatusVariant
> = {
  queued: 'secondary',
  running: 'secondary',
  waiting: 'warning',
  succeeded: 'success',
  failed: 'destructive',
  cancelled: 'outline',
}

/** Badge variant for a status. The label always accompanies the color. */
export function operationStatusVariant(
  status: OperationStatus
): OperationStatusVariant {
  return OPERATION_STATUS_VARIANT[status] ?? 'secondary'
}

export const OPERATION_KIND_LABEL: Record<OperationKind, string> = {
  deployment: 'Deployment',
  rollback: 'Rollback',
  promotion: 'Promotion',
  restore: 'Restore',
  backup: 'Backup',
  autofix: 'Autofix',
}

export const OPERATION_KIND_ICON: Record<OperationKind, LucideIcon> = {
  deployment: Rocket,
  rollback: RotateCcw,
  promotion: ArrowUpRight,
  restore: DatabaseBackup,
  backup: Archive,
  autofix: Wand2,
}

/** Icon for a client-only (not persisted) entry such as a container restart. */
export const LOCAL_OPERATION_ICON: LucideIcon = RefreshCw

/**
 * Secondary line for a row: where the operation runs. Project and environment
 * for deployment-like work, the service for storage work.
 */
export function operationContext(operation: OperationEntry): string | null {
  const parts: string[] = []
  if (operation.project_slug) parts.push(operation.project_slug)
  if (operation.environment_name) parts.push(operation.environment_name)
  if (operation.service_name && operation.kind !== 'backup') {
    parts.push(operation.service_name)
  }
  if (parts.length === 0 && operation.service_name) {
    parts.push(operation.service_name)
  }
  return parts.length > 0 ? parts.join(' · ') : null
}

/**
 * Timestamp that best describes the row: when it finished for finished work,
 * otherwise when it started (or was created, if it hasn't started yet).
 */
export function operationTimestamp(operation: OperationEntry): string {
  if (!isActiveOperationStatus(operation.status) && operation.finished_at) {
    return operation.finished_at
  }
  return operation.started_at ?? operation.created_at
}

/** Compact relative time: "just now", "4m ago", "3h ago", "2d ago". */
export function formatRelativeShort(iso: string, now: number): string {
  const then = Date.parse(iso)
  if (Number.isNaN(then)) return ''
  const seconds = Math.max(0, Math.round((now - then) / 1000))
  if (seconds < 45) return 'just now'
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.round(hours / 24)
  return `${days}d ago`
}

export interface OperationGroups {
  active: OperationEntry[]
  finished: OperationEntry[]
}

/**
 * Split the feed into in-flight and finished work, preserving the server's
 * newest-first order inside each group.
 */
export function groupOperations(
  operations: readonly OperationEntry[]
): OperationGroups {
  const active: OperationEntry[] = []
  const finished: OperationEntry[] = []
  for (const operation of operations) {
    if (isActiveOperationStatus(operation.status)) active.push(operation)
    else finished.push(operation)
  }
  return { active, finished }
}

/** Accessible name for the header trigger. */
export function operationsTriggerLabel(runningCount: number): string {
  return runningCount > 0
    ? `Operations (${runningCount} running)`
    : 'Operations'
}

/** Badge text: hidden at zero, capped at 9+. */
export function operationsBadgeText(runningCount: number): string | null {
  if (runningCount <= 0) return null
  return runningCount > 9 ? '9+' : String(runningCount)
}
