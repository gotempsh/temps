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
  OperationsListResponse,
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

/**
 * Page size for in-flight work. The API maximum, so a single request almost
 * always holds every counted operation; further pages are fetched on demand.
 */
export const OPERATIONS_RUNNING_PAGE_SIZE = 100

/** Page size for finished history. The API's default page size. */
export const OPERATIONS_HISTORY_PAGE_SIZE = 20

/**
 * Query for the tray's "Running" section. Fetched separately from history so
 * an old in-flight operation can never be pushed off the list by newer
 * finished ones: every operation the badge counts is in this result set.
 */
export const RUNNING_OPERATIONS_QUERY = {
  status: 'running',
  page_size: OPERATIONS_RUNNING_PAGE_SIZE,
} as const

/** Query for the tray's "Recent" (finished) section. */
export const FINISHED_OPERATIONS_QUERY = {
  status: 'finished',
  page_size: OPERATIONS_HISTORY_PAGE_SIZE,
} as const

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

/**
 * `getNextPageParam` for the feed: the next 1-based page, or `undefined` once
 * every operation matching the query has been loaded.
 */
export function operationsNextPage(
  lastPage: OperationsListResponse
): number | undefined {
  if (lastPage.operations.length === 0) return undefined
  const loadedThrough = lastPage.page * lastPage.page_size
  return loadedThrough < lastPage.total ? lastPage.page + 1 : undefined
}

/**
 * Concatenate feed pages newest-first, dropping repeated ids (offset paging
 * can repeat a row when new operations arrive between page fetches) and any
 * id in `exclude`.
 */
export function flattenOperationPages(
  pages: readonly OperationsListResponse[] | undefined,
  exclude?: ReadonlySet<string>
): OperationEntry[] {
  const seen = new Set<string>(exclude)
  const result: OperationEntry[] = []
  for (const page of pages ?? []) {
    for (const operation of page.operations) {
      if (seen.has(operation.id)) continue
      seen.add(operation.id)
      result.push(operation)
    }
  }
  return result
}

export interface OperationsTrayFeed {
  /** Every loaded in-flight operation, newest first. */
  running: OperationEntry[]
  /** Loaded finished operations, newest first, never repeating a running row. */
  recent: OperationEntry[]
  /** Server-side in-flight count. Drives the badge. */
  runningCount: number
  /** In-flight operations the badge counts that are not loaded as rows yet. */
  runningNotLoaded: number
}

/**
 * Derive the tray's sections from the running and finished feeds. The badge
 * count and the running rows come from the same (`status=running`) response,
 * so a counted operation is always either a row or covered by
 * `runningNotLoaded`, which the tray offers to load.
 */
export function operationsTrayFeed({
  runningPages,
  finishedPages,
}: {
  runningPages: readonly OperationsListResponse[] | undefined
  finishedPages: readonly OperationsListResponse[] | undefined
}): OperationsTrayFeed {
  const running = flattenOperationPages(runningPages)
  // A row that just finished may briefly appear in both feeds; show it once,
  // where the badge counts it, until the next poll moves it.
  const recent = flattenOperationPages(
    finishedPages,
    new Set(running.map((operation) => operation.id))
  )
  const runningCount = runningPages?.[0]?.running_count ?? 0
  return {
    running,
    recent,
    runningCount,
    runningNotLoaded: Math.max(0, runningCount - running.length),
  }
}

/**
 * Whether any operation in `previousIds` is missing from `currentIds`, i.e.
 * something finished (or was cancelled) between two running-feed responses.
 */
export function operationsLeftRunning(
  previousIds: readonly string[],
  currentIds: readonly string[]
): boolean {
  if (previousIds.length === 0) return false
  const current = new Set(currentIds)
  return previousIds.some((id) => !current.has(id))
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
