// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for the child-backup listing endpoint.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   GET /backups/{id}/children
 * once `bun run openapi-ts` is re-run against a server that exposes this
 * endpoint.
 *
 * Pattern mirrors `external-service-backups.ts` and `schedule-runs.ts`.
 */

/** A single child backup entry returned by GET /backups/{id}/children. */
export interface ChildBackupEntry {
  /** Row ID from `external_service_backups`. */
  id: number
  /** FK to `external_services.id`. */
  service_id: number
  /** Human-readable name of the external service (e.g. "redis-prod"). */
  service_name: string
  /** Service type string (e.g. "postgres", "redis", "mongodb", "s3"). */
  service_type: string
  /** Current state: "pending" | "running" | "completed" | "failed" */
  state: string
  /** Backup variant (e.g. "full", "incremental"). */
  backup_type: string
  /** ISO 8601 / RFC 3339 timestamp. */
  started_at: string
  /** ISO 8601 / RFC 3339 timestamp; null when backup is still running. */
  finished_at: string | null
  /** Size of the child backup in bytes; null when not yet known. */
  size_bytes: number | null
  /** Object key or s3:// URL where the backup data lives. */
  s3_location: string
  /** Engine-reported error message when state = "failed". */
  error_message: string | null
  /** Compression algorithm used (e.g. "gzip", "lz4"). */
  compression_type: string
}

/** Response body for GET /backups/{id}/children. */
export interface ChildBackupListResponse {
  children: ChildBackupEntry[]
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
 * Fetch the list of external-service child backups for a parent backup row.
 *
 * Returns `{ children: [] }` — not a 404 — when the parent has no children.
 * Throws when the parent backup does not exist (server returns 404).
 */
export async function listBackupChildren(
  parentBackupId: number,
): Promise<ChildBackupListResponse> {
  const response = await fetch(`/api/backups/${parentBackupId}/children`, {
    credentials: 'include',
  })
  return readJsonOrThrow<ChildBackupListResponse>(response)
}

/**
 * Returns TanStack Query `queryKey` + `queryFn` options for
 * `listBackupChildren`, compatible with `useQuery`.
 *
 * Note: `parentBackupId` is the integer row id from the `backups` table (not
 * the UUID string used in the URL for `GET /backups/{uuid}`).
 */
export function listBackupChildrenOptions(parentBackupId: number | undefined) {
  return {
    queryKey: ['backup-children', parentBackupId] as const,
    queryFn: () => listBackupChildren(parentBackupId!),
    enabled: parentBackupId !== undefined,
  }
}
