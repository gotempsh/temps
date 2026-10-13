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

export interface WalHealthResponse {
  wal_health: PostgresWalHealth | null
}

export async function getPostgresWalHealth(
  id: number
): Promise<WalHealthResponse> {
  const { data, error, response } = await getPostgresWalHealthRequest({
    path: { id },
  })
  if (response?.status === 404) return { wal_health: null }
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
