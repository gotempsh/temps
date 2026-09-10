// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for the backup-alerts endpoint.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   GET /backups/alerts
 * once `bun run openapi-ts` is re-run against a server that exposes this
 * endpoint.
 */

/** A single open backup alert returned by the watcher. */
export interface BackupAlertResponse {
  id: number
  /** "overdue_schedule" or "stalled_job" */
  kind: 'overdue_schedule' | 'stalled_job'
  /** "warning" or "critical" */
  severity: 'warning' | 'critical'
  schedule_id: number | null
  /** Human-readable schedule name, populated when kind === "overdue_schedule" */
  schedule_name: string | null
  /** s3_source_id of the schedule — used to deep-link to the source detail page. */
  schedule_s3_source_id: number | null
  job_id: number | null
  /** Parent `backups.id` for stalled_job alerts. */
  backup_id: number | null
  /** s3_source_id of the parent backup — lets the UI link to the backup detail page. */
  backup_s3_source_id: number | null
  message: string
  /** RFC 3339 timestamp */
  opened_at: string
}

export interface BackupAlertListResponse {
  alerts: BackupAlertResponse[]
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
 * Fetch all currently open (unresolved) backup alerts, newest first.
 *
 * Returns an empty list when everything is healthy. Alerts are auto-resolved
 * by the server-side watcher — no client-side dismiss is needed.
 */
export async function listBackupAlerts(): Promise<BackupAlertListResponse> {
  const response = await fetch('/api/backups/alerts', {
    credentials: 'include',
  })
  return readJsonOrThrow<BackupAlertListResponse>(response)
}

/**
 * Returns TanStack Query `queryKey` + `queryFn` options for
 * `listBackupAlerts`, compatible with `useQuery`.
 *
 * Polls every 60 seconds so the banner disappears automatically once the
 * watcher resolves an alert (at most one watcher interval of lag).
 */
export function listBackupAlertsOptions() {
  return {
    queryKey: ['backup-alerts'] as const,
    queryFn: () => listBackupAlerts(),
    refetchInterval: 60_000,
  }
}
