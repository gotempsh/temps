// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for PostgreSQL major-upgrade endpoints, pending
 * regeneration of the OpenAPI client. Once `bun run openapi-ts` is re-run
 * against a server that exposes these endpoints, this file can be deleted
 * and the generated SDK used directly.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   - GET  /external-services/{service_id}/upgrades
 *   - GET  /external-services/{service_id}/upgrades/{upgrade_id}
 *   - GET  /external-services/{service_id}/upgrades/{upgrade_id}/logs
 *   - POST /external-services/{service_id}/upgrades
 *   - POST /external-services/{service_id}/upgrades/{upgrade_id}/retry
 *   - POST /external-services/{service_id}/upgrades/{upgrade_id}/cancel
 * once these endpoints are included in the OpenAPI spec / generated client.
 */

export interface PgUpgrade {
  id: number
  service_id: number
  from_version: string
  to_version: string
  from_image: string
  to_image: string
  status: string
  phase: string
  log_id: string
  pre_upgrade_backup_id: number | null
  rollback_volume_name: string | null
  error_message: string | null
  attempt: number
  started_at: string | null
  finished_at: string | null
  created_at: string
}

export interface PgUpgradeLog {
  log_id: string
  content: string
}

export interface StartPgUpgradeBody {
  from_version: string
  to_version: string
  from_image: string
  to_image: string
}

async function readJsonOrThrow<T>(response: Response): Promise<T> {
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as { detail?: string; title?: string }
      detail = body.detail || body.title || detail
    } catch {
      // fall through with statusText
    }
    throw new Error(detail)
  }
  return (await response.json()) as T
}

export async function listPgUpgrades(serviceId: number): Promise<PgUpgrade[]> {
  const response = await fetch(`/api/external-services/${serviceId}/upgrades`, {
    credentials: 'include',
  })
  return readJsonOrThrow<PgUpgrade[]>(response)
}

export async function getPgUpgrade(
  serviceId: number,
  upgradeId: number,
): Promise<PgUpgrade> {
  const response = await fetch(
    `/api/external-services/${serviceId}/upgrades/${upgradeId}`,
    { credentials: 'include' },
  )
  return readJsonOrThrow<PgUpgrade>(response)
}

export async function getPgUpgradeLogs(
  serviceId: number,
  upgradeId: number,
): Promise<PgUpgradeLog> {
  const response = await fetch(
    `/api/external-services/${serviceId}/upgrades/${upgradeId}/logs`,
    { credentials: 'include' },
  )
  return readJsonOrThrow<PgUpgradeLog>(response)
}

export async function startPgUpgrade(
  serviceId: number,
  body: StartPgUpgradeBody,
): Promise<PgUpgrade> {
  const response = await fetch(`/api/external-services/${serviceId}/upgrades`, {
    method: 'POST',
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  return readJsonOrThrow<PgUpgrade>(response)
}

export async function retryPgUpgrade(
  serviceId: number,
  upgradeId: number,
): Promise<PgUpgrade> {
  const response = await fetch(
    `/api/external-services/${serviceId}/upgrades/${upgradeId}/retry`,
    { method: 'POST', credentials: 'include' },
  )
  return readJsonOrThrow<PgUpgrade>(response)
}

export async function cancelPgUpgrade(
  serviceId: number,
  upgradeId: number,
): Promise<PgUpgrade> {
  const response = await fetch(
    `/api/external-services/${serviceId}/upgrades/${upgradeId}/cancel`,
    { method: 'POST', credentials: 'include' },
  )
  return readJsonOrThrow<PgUpgrade>(response)
}

/**
 * Roll back a completed major upgrade to the pre-upgrade PGDATA volume.
 *
 * Throws the raw problem-details object rather than a plain `Error` so that
 * `handleSensitiveActionError` can detect a STEP_UP_REQUIRED response and
 * open the MFA verification dialog.
 */
export async function rollbackPgUpgrade(
  serviceId: number,
  upgradeId: number,
): Promise<PgUpgrade> {
  const response = await fetch(
    `/api/external-services/${serviceId}/upgrades/${upgradeId}/rollback`,
    { method: 'POST', credentials: 'include' },
  )
  if (!response.ok) {
    let problem: Record<string, unknown> = { status: response.status }
    try {
      problem = (await response.json()) as Record<string, unknown>
    } catch {
      problem['detail'] = response.statusText
    }
    // eslint-disable-next-line @typescript-eslint/no-throw-literal
    throw problem
  }
  return (await response.json()) as PgUpgrade
}

// ---- Phase timeline helpers -------------------------------------------

export const PG_UPGRADE_PHASES = [
  'pre_backup',
  'snapshot',
  'dump',
  'new_container',
  'restore',
  'swap',
  'analyze',
  'completed',
] as const

export type PgUpgradePhase = (typeof PG_UPGRADE_PHASES)[number]

export const PHASE_LABELS: Record<PgUpgradePhase, string> = {
  pre_backup: 'Pre-upgrade backup',
  snapshot: 'Snapshot volume',
  dump: 'Dump old database',
  new_container: 'Create new container',
  restore: 'Restore into new version',
  swap: 'Swap containers',
  analyze: 'ANALYZE planner stats',
  completed: 'Completed',
}

export function phaseIndex(phase: string): number {
  const idx = PG_UPGRADE_PHASES.indexOf(phase as PgUpgradePhase)
  return idx === -1 ? 0 : idx
}

export function isTerminal(status: string): boolean {
  return status === 'completed' || status === 'failed' || status === 'cancelled'
}
