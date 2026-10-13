// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for the per-service backup listing endpoint.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   GET /backups/external-services/{service_id}/backups
 * once `bun run openapi-ts` is re-run against a server that exposes this
 * endpoint.
 */

export interface ServiceBackupEntry {
  id: number
  backup_id: string
  name: string
  state: string
  backup_type: string
  /** ISO 8601 timestamp */
  started_at: string
  /** ISO 8601 timestamp, null when backup is still running */
  finished_at: string | null
  size_bytes: number | null
  s3_location: string
  error_message: string | null
  compression_type: string
  s3_source_id: number
  s3_source_name: string
  external_service_backup_id: number
}

export interface ServiceBackupListResponse {
  backups: ServiceBackupEntry[]
  total: number
  page: number
  page_size: number
}

async function readJsonOrThrow<T>(response: Response): Promise<T> {
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as {
        detail?: string
        title?: string
      }
      detail = body.detail ?? body.title ?? detail
    } catch {
      // fall through with statusText
    }
    throw new Error(detail)
  }
  return (await response.json()) as T
}

/**
 * Fetch a page of backups for a specific external service.
 * Never triggers an S3 scan — always returns DB-only results in <100 ms.
 */
export async function listExternalServiceBackups(
  serviceId: number,
  page = 1,
  pageSize = 20
): Promise<ServiceBackupListResponse> {
  const params = new URLSearchParams({
    page: String(page),
    page_size: String(pageSize),
  })
  const response = await fetch(
    `/api/backups/external-services/${serviceId}/backups?${params}`,
    { credentials: 'include' }
  )
  return readJsonOrThrow<ServiceBackupListResponse>(response)
}

/**
 * Key prefix shared by every page of one service's backup list. Invalidate
 * it after enqueuing a backup so the new `pending` row shows immediately.
 */
export function externalServiceBackupsQueryKey(serviceId: number | undefined) {
  return ['external-service-backups', serviceId] as const
}

/** Poll cadence while a listed backup is still queued or running. */
export const ACTIVE_BACKUP_POLL_INTERVAL_MS = 3000

const ACTIVE_BACKUP_STATES = new Set(['pending', 'running'])

/**
 * `refetchInterval` for the backup list: poll only while some listed backup
 * has not reached a terminal state, so the card moves from queued to
 * completed/failed on its own and stops polling once nothing is in flight.
 */
export function externalServiceBackupsRefetchInterval(
  data: ServiceBackupListResponse | undefined
): number | false {
  return data?.backups.some((backup) => ACTIVE_BACKUP_STATES.has(backup.state))
    ? ACTIVE_BACKUP_POLL_INTERVAL_MS
    : false
}

/**
 * Returns TanStack Query `queryKey` + `queryFn` options for
 * `listExternalServiceBackups`, compatible with `useQuery`.
 */
export function listExternalServiceBackupsOptions(
  serviceId: number | undefined,
  page = 1,
  pageSize = 20
) {
  return {
    queryKey: [
      ...externalServiceBackupsQueryKey(serviceId),
      page,
      pageSize,
    ] as const,
    queryFn: () => listExternalServiceBackups(serviceId!, page, pageSize),
    enabled: serviceId !== undefined,
    refetchInterval: (query: { state: { data?: ServiceBackupListResponse } }) =>
      externalServiceBackupsRefetchInterval(query.state.data),
  }
}
