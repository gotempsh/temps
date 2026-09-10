// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written client for the Postgres WAL health endpoint. Mirrors the schema
 * defined in `temps_providers::externalsvc::postgres_wal_health`.
 *
 * TODO(sdk-regen): replace with generated helpers for
 *   - GET /external-services/{id}/wal-health
 */

export type ArchiveMode = 'off' | 'on' | 'always' | 'unknown'

export type WalWarningSeverity = 'warning' | 'critical'

export interface StaleSlot {
  slot_name: string
  active: boolean
  retained_bytes: number
}

export type WalWarning =
  | {
      kind: 'wal_bloat'
      pg_wal_bytes: number
      max_wal_size_bytes: number
      ratio: number
    }
  | {
      kind: 'stale_slot'
      slot_name: string
      retained_bytes: number
      active: boolean
    }
  | { kind: 'archive_backlog'; ready_count: number }
  | { kind: 'archive_mode_without_command' }
  | { kind: 'wal_not_recycled'; oldest_age_secs: number }

export interface PostgresWalHealth {
  probed_at: string
  pg_wal_bytes: number
  max_wal_size_bytes: number
  archive_mode: ArchiveMode
  archive_command: string | null
  archive_backlog: number
  archiver_failed_count: number | null
  archiver_last_failed_at: string | null
  stale_slots: StaleSlot[]
  oldest_wal_age_secs: number
  warnings: WalWarning[]
}

export interface WalHealthResponse {
  wal_health: PostgresWalHealth | null
}

export async function getPostgresWalHealth(
  id: number
): Promise<WalHealthResponse> {
  const response = await fetch(`/api/external-services/${id}/wal-health`, {
    credentials: 'include',
  })
  if (!response.ok) {
    // 404 here means "not a Postgres service" or "service not found" — both are
    // expected, non-error states for the panel. Let the caller decide via the
    // status code instead of throwing.
    if (response.status === 404) {
      return { wal_health: null }
    }
    let detail = response.statusText
    try {
      const body = (await response.json()) as {
        detail?: string
        title?: string
      }
      detail = body.detail || body.title || detail
    } catch {
      // fall through
    }
    throw new Error(detail)
  }
  // Backend returns the snapshot directly (not wrapped). Wrap it client-side
  // so the panel can treat 404 and "no warnings" with one code path.
  const snapshot = (await response.json()) as PostgresWalHealth
  return { wal_health: snapshot }
}

export function severityOf(warning: WalWarning): WalWarningSeverity {
  switch (warning.kind) {
    case 'wal_bloat':
      return warning.ratio >= 10 ? 'critical' : 'warning'
    case 'stale_slot':
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
