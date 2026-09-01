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
      const body = (await response.json()) as { detail?: string; title?: string }
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
  pageSize = 20,
): Promise<ServiceBackupListResponse> {
  const params = new URLSearchParams({
    page: String(page),
    page_size: String(pageSize),
  })
  const response = await fetch(
    `/api/backups/external-services/${serviceId}/backups?${params}`,
    { credentials: 'include' },
  )
  return readJsonOrThrow<ServiceBackupListResponse>(response)
}

/**
 * Returns TanStack Query `queryKey` + `queryFn` options for
 * `listExternalServiceBackups`, compatible with `useQuery`.
 */
export function listExternalServiceBackupsOptions(
  serviceId: number | undefined,
  page = 1,
  pageSize = 20,
) {
  return {
    queryKey: ['external-service-backups', serviceId, page, pageSize] as const,
    queryFn: () => listExternalServiceBackups(serviceId!, page, pageSize),
    enabled: serviceId !== undefined,
  }
}
