// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getPostgresWalHealth as getPostgresWalHealthRequest } from '@/api/client/sdk.gen'
import type {
  PostgresWalHealth,
  WalWarning,
  WalWarningSeverity,
} from '@/api/client/types.gen'
import { problemDetail } from '@/lib/api-problem'

export type {
  ArchiveMode,
  PostgresWalHealth,
  StaleSlot,
  WalWarning,
  WalWarningSeverity,
} from '@/api/client/types.gen'

/** Problem type of a 404 whose last WAL check could not run. */
export const WAL_HEALTH_NOT_CHECKED_TYPE =
  'https://temps.sh/probs/wal-health-not-checked'

/** Why the server is not checking WAL health for this database. */
export interface WalHealthNotChecked {
  reason: string
  checked_at: string | null
}

export interface WalHealthResponse {
  wal_health: PostgresWalHealth | null
  /** Set when the last check could not run, e.g. a database on a worker
   *  whose agent predates the WAL probe. */
  not_checked?: WalHealthNotChecked | null
}

/** The not-checked reason a 404 Problem carries, if it is one. */
export function notCheckedFromProblem(
  problem: unknown
): WalHealthNotChecked | null {
  if (!problem || typeof problem !== 'object') return null
  const record = problem as Record<string, unknown>
  if (record.type !== WAL_HEALTH_NOT_CHECKED_TYPE) return null
  const reason =
    typeof record.reason === 'string'
      ? record.reason
      : typeof record.detail === 'string'
        ? record.detail
        : null
  if (!reason) return null
  return {
    reason,
    checked_at:
      typeof record.checked_at === 'string' ? record.checked_at : null,
  }
}

export async function getPostgresWalHealth(
  id: number
): Promise<WalHealthResponse> {
  const { data, error, response } = await getPostgresWalHealthRequest({
    path: { id },
  })
  if (response?.status === 404)
    return { wal_health: null, not_checked: notCheckedFromProblem(error) }
  if (error)
    throw new Error(
      problemDetail(error, 'Unable to check PostgreSQL WAL health.')
    )
  return { wal_health: data ?? null }
}

export function severityOf(warning: WalWarning): WalWarningSeverity {
  switch (warning.kind) {
    case 'wal_bloat':
      return warning.ratio >= 10 ? 'critical' : 'warning'
    case 'stale_slot':
    case 'archive_failing':
      return 'critical'
    default:
      return 'warning'
  }
}

/** Pretty-print a byte count for the alert body. */
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let n = bytes
  let unit = 0
  while (n >= 1024 && unit < units.length - 1) {
    n /= 1024
    unit++
  }
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[unit]}`
}

export type ArchivingState = 'archiving' | 'failing' | 'waiting' | 'discarding'

/** Stall threshold, the server's `ARCHIVE_STALL_SECS`. */
export const ARCHIVE_STALL_SECS = 300

/**
 * Discarding when `archive_command` is empty or a no-op such as
 * `/bin/true`: PostgreSQL counts every segment as archived and recycles it,
 * so `pg_stat_archiver` reads as healthy while nothing is stored (the
 * server reports this as the `archive_mode_without_command` warning).
 * Failing when the last failure is newer than the last success, or when a
 * segment has waited five minutes (WAL-G retrying without failing yet).
 */
export function archivingState(snapshot: PostgresWalHealth): ArchivingState {
  if (snapshot.warnings.some((w) => w.kind === 'archive_mode_without_command'))
    return 'discarding'
  const failedAt = snapshot.archiver_last_failed_at
  const archivedAt = snapshot.archiver_last_archived_at
  if (
    failedAt &&
    (!archivedAt || Date.parse(failedAt) > Date.parse(archivedAt))
  )
    return 'failing'
  if ((snapshot.archive_oldest_ready_age_secs ?? 0) >= ARCHIVE_STALL_SECS)
    return 'failing'
  return archivedAt ? 'archiving' : 'waiting'
}
