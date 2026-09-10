// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for the schedule run-history and run-now endpoints.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   GET /backups/schedules/{id}/runs
 *   POST /backups/schedules/{id}/run
 * once `bun run openapi-ts` is re-run against a server that exposes these
 * endpoints.
 *
 * Pattern mirrors `external-service-backups.ts`.
 */

/**
 * One scheduler tick (fan-out). A tick spawns N child `backups` rows
 * (control plane + every external service). Aggregate state is computed
 * from child counts; see backend `ScheduleRunSummary`.
 */
export interface ScheduleRunSummary {
  /**
   * `schedule_runs.id` for this tick. Negative values represent synthetic
   * rows for legacy pre-fan-out backups (no `schedule_run_id`).
   */
  run_id: number
  /** FK to `backup_schedules.id`. */
  schedule_id: number
  /** How the run was triggered: `"cron"` or `"manual"`. */
  triggered_by: string
  /** ISO 8601 / RFC 3339 timestamp when the fan-out started. */
  started_at: string
  /** ISO 8601 / RFC 3339 timestamp once all children reached a terminal state. */
  finished_at: string | null
  /**
   * Aggregate state computed from child counts:
   * - `"running"` — at least one child pending/running
   * - `"failed"` — at least one child failed and none running
   * - `"completed"` — all children completed
   */
  aggregate_state: string
  /** Total number of child backup jobs in this run. */
  total_jobs: number
  /** Number of children in `state = "completed"`. */
  completed_jobs: number
  /** Number of children in `state = "failed"`. */
  failed_jobs: number
  /** Number of children in `state = "running"`. */
  running_jobs: number
  /** Number of children in `state = "pending"`. */
  pending_jobs: number
}

/** Paginated run-history response for a backup schedule. */
export interface ScheduleRunListResponse {
  runs: ScheduleRunSummary[]
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
 * Fetch a page of run-history entries for a specific backup schedule.
 *
 * Never triggers an S3 scan — returns DB-only results ordered by
 * `started_at DESC` (newest first).
 */
export async function listScheduleRuns(
  scheduleId: number,
  page = 1,
  pageSize = 20,
): Promise<ScheduleRunListResponse> {
  const params = new URLSearchParams({
    page: String(page),
    page_size: String(pageSize),
  })
  const response = await fetch(
    `/api/backups/schedules/${scheduleId}/runs?${params}`,
    { credentials: 'include' },
  )
  return readJsonOrThrow<ScheduleRunListResponse>(response)
}

/**
 * Returns TanStack Query `queryKey` + `queryFn` options for
 * `listScheduleRuns`, compatible with `useQuery`.
 */
export function listScheduleRunsOptions(
  scheduleId: number | undefined,
  page = 1,
  pageSize = 20,
) {
  return {
    queryKey: ['schedule-runs', scheduleId, page, pageSize] as const,
    queryFn: () => listScheduleRuns(scheduleId!, page, pageSize),
    enabled: scheduleId !== undefined,
  }
}

/**
 * One child backup job within a scheduler tick. Returned by
 * `GET /api/backups/schedule-runs/{run_id}/jobs`.
 */
export interface ScheduleRunJobEntry {
  /** `backups.id` for this job. */
  backup_id: number
  /** `backups.backup_id` UUID string — used to link to the backup detail page. */
  backup_uuid: string
  /** Engine key (e.g. `"control_plane"`, `"redis"`). */
  engine: string
  /** Display name — service name, or `"control plane"`. */
  service_name: string
  /** `external_services.id` — null for the control-plane job. */
  service_id: number | null
  /** Current state: `"pending"` | `"running"` | `"completed"` | `"failed"`. */
  state: string
  /** ISO 8601 / RFC 3339 timestamp. */
  started_at: string
  /** ISO 8601 / RFC 3339 timestamp; null while running. */
  finished_at: string | null
  /** Size in bytes once completed; null while running. */
  size_bytes: number | null
  /** Engine-reported error message when `state = "failed"`. */
  error_message: string | null
  /** FK to `s3_sources.id` — used to build the backup-detail link. */
  s3_source_id: number
}

/**
 * Fetch the per-job list for a single scheduler tick.
 *
 * The endpoint returns a bare JSON array of job entries (pagination params
 * are honoured server-side but the response shape is just the array).
 *
 * Negative `runId` values represent synthetic legacy rows (pre-fan-out
 * backups) and are not supported by this endpoint — callers should
 * short-circuit on negative ids and fall back to the legacy backup link.
 */
export async function listScheduleRunJobs(
  runId: number,
  page = 1,
  pageSize = 50,
): Promise<ScheduleRunJobEntry[]> {
  const params = new URLSearchParams({
    page: String(page),
    page_size: String(pageSize),
  })
  const response = await fetch(
    `/api/backups/schedule-runs/${runId}/jobs?${params}`,
    { credentials: 'include' },
  )
  return readJsonOrThrow<ScheduleRunJobEntry[]>(response)
}

export function listScheduleRunJobsOptions(
  runId: number | undefined,
  page = 1,
  pageSize = 50,
) {
  return {
    queryKey: ['schedule-run-jobs', runId, page, pageSize] as const,
    queryFn: () => listScheduleRunJobs(runId!, page, pageSize),
    // Negative ids are synthetic legacy rows — don't query the endpoint.
    enabled: runId !== undefined && runId > 0,
  }
}

/** One enqueued backup_jobs row returned by the fan-out trigger. */
export interface EnqueuedJob {
  /** `backup_jobs.id` for the newly enqueued job. */
  job_id: number
  /** `backups.id` of the parent backup row. */
  backup_id: number
  /** `backups.backup_id` UUID string. */
  backup_uuid: string
  /** Engine key (`"control_plane"`, `"redis"`, etc.). */
  engine: string
  /** Display name — service name, or `"control plane"`. */
  service_name: string
}

/** Response body for `POST /api/backups/schedules/{id}/run` (fan-out). */
export interface ScheduleRunResponse {
  /** The `schedule_runs.id` of the newly created run. */
  schedule_run_id: number
  /** Every job enqueued in this fan-out. */
  jobs: EnqueuedJob[]
}

/**
 * Immediately enqueue a fan-out run for the given schedule (Run Now).
 *
 * Creates one `schedule_runs` row and one backup job per supported target
 * (control plane + every external service). Returns the run id plus the
 * list of enqueued jobs so callers can show progress immediately.
 *
 * Throws with a descriptive message on failure (including 409 Conflict when
 * a run is already in flight or the schedule is disabled).
 */
export async function runScheduleNow(
  scheduleId: number,
): Promise<ScheduleRunResponse> {
  const response = await fetch(
    `/api/backups/schedules/${scheduleId}/run`,
    {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
    },
  )
  return readJsonOrThrow<ScheduleRunResponse>(response)
}

/** Response from cancel endpoints. */
export interface CancelBackupResponse {
  /**
   * Number of rows actually flipped to `failed`. `0` is a valid success and
   * means the target was already terminal — cancel is idempotent.
   */
  cancelled: number
}

/**
 * Cancel one in-flight backup by `backups.id`. Sets the in-process
 * cancellation token + flips the DB row to `failed` with a "cancelled" reason.
 * Returns `cancelled: 0` if the backup was already terminal.
 */
export async function cancelBackup(
  backupId: number,
): Promise<CancelBackupResponse> {
  const response = await fetch(`/api/backups/${backupId}/cancel`, {
    method: 'POST',
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
  })
  return readJsonOrThrow<CancelBackupResponse>(response)
}

/**
 * Cancel every non-terminal child backup belonging to a scheduler run.
 * Closes the parent `schedule_runs` row once no live children remain.
 * Returns the number of children cancelled.
 */
export async function cancelScheduleRun(
  runId: number,
): Promise<CancelBackupResponse> {
  const response = await fetch(
    `/api/backups/schedule-runs/${runId}/cancel`,
    {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
    },
  )
  return readJsonOrThrow<CancelBackupResponse>(response)
}
