// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for the bulk Cloud-telemetry activation surface (ADR-042 §11).
 *
 * Everything here is deliberately free of React so the rules that are easy to
 * get subtly wrong — "an omitted percentage is not 0%", "an ETA is only a
 * number when the server says `known`", "`switching` and `backfilling` are not
 * the same thing" — are testable in isolation rather than buried in JSX.
 */

import type {
  BulkActivationEtaState,
  BulkActivationJobProjectResponse,
  BulkActivationJobResponse,
  BulkJobProjectStatus,
  BulkJobStatus,
} from '@/api/client/types.gen'

// ---------------------------------------------------------------------------
// Job lifecycle
// ---------------------------------------------------------------------------

/**
 * Statuses that will never change again, so polling must stop.
 *
 * `aborted` is terminal for *this* job even though its untouched projects are
 * still `pending` — resuming starts a new job, it does not revive this one.
 */
const TERMINAL_JOB_STATUSES: readonly BulkJobStatus[] = [
  'completed',
  'completed_with_failures',
  'aborted',
  'cancelled',
]

export function isTerminalJobStatus(status: BulkJobStatus): boolean {
  return TERMINAL_JOB_STATUSES.includes(status)
}

export function isJobActive(
  job: BulkActivationJobResponse | null | undefined
): boolean {
  return !!job && !isTerminalJobStatus(job.status)
}

/** Human label for a job status. Each of the six says something different. */
export const JOB_STATUS_LABELS: Record<BulkJobStatus, string> = {
  pending: 'Queued',
  running: 'Running',
  completed: 'Completed',
  completed_with_failures: 'Completed with failures',
  aborted: 'Stopped',
  cancelled: 'Cancelled',
}

/**
 * One sentence explaining what a terminal status means for the operator.
 *
 * `aborted` and `cancelled` are not collapsed: one was the instance's fault and
 * resumes, the other was asked for.
 */
export const JOB_STATUS_DETAIL: Record<BulkJobStatus, string> = {
  pending: 'Queued. The activation starts as soon as the worker picks it up.',
  running: 'Switching projects to Temps Cloud and shipping their history.',
  completed: 'Every project in this activation is on Temps Cloud.',
  completed_with_failures:
    'The activation ran to the end, but some projects did not finish. Each one is listed below with its reason.',
  aborted:
    'An instance-wide condition stopped this activation. Projects it never reached are still queued, so resuming picks up where it left off — nothing already paid for is sent twice.',
  cancelled:
    'Someone asked this activation to stop. Progress is durable: resuming re-sends nothing that already reached Cloud.',
}

// ---------------------------------------------------------------------------
// Per-project status
// ---------------------------------------------------------------------------

export const PROJECT_STATUS_LABELS: Record<BulkJobProjectStatus, string> = {
  pending: 'Queued',
  switching: 'Switching',
  backfilling: 'Backfilling',
  done: 'Done',
  failed: 'Failed',
  skipped: 'Skipped',
}

/**
 * Why `switching` and `backfilling` are rendered as different things.
 *
 * The switch is instant and egresses nothing; the backfill can run for hours
 * and costs money. An operator watching a job that looks stuck needs to know
 * which of the two it is stuck in.
 */
export const PROJECT_STATUS_HINTS: Record<BulkJobProjectStatus, string> = {
  pending: 'Not started yet — nothing has been sent for this project.',
  switching:
    'Pointing new spans at Temps Cloud. Instant, and it sends no history.',
  backfilling:
    'Sending this project’s existing history. This is the part that ' +
    'takes time and costs egress.',
  done: 'Switched, and its history has been shipped.',
  failed:
    'History backfill failed. New spans are still going to Temps Cloud — the switch is never rolled back — so this is a recorded hole in history, not a broken project.',
  skipped: 'Not eligible, so nothing was switched and nothing was sent.',
}

/** Tailwind classes that keep `switching` and `backfilling` visually distinct. */
export const PROJECT_STATUS_CLASSES: Record<BulkJobProjectStatus, string> = {
  pending: 'border-border text-muted-foreground',
  switching:
    'border-sky-400 text-sky-700 dark:border-sky-500 dark:text-sky-400',
  backfilling:
    'border-amber-400 bg-amber-50 text-amber-700 dark:border-amber-500 dark:bg-amber-950/30 dark:text-amber-400',
  done: 'border-green-500 text-green-700 dark:text-green-500',
  failed: 'border-destructive text-destructive',
  skipped: 'border-dashed border-border text-muted-foreground',
}

/**
 * The reason a project was skipped, as prose.
 *
 * `skip_detail` is written by the server and is rendered **verbatim**. When it
 * is missing the raw token is shown rather than a client-invented sentence: a
 * token this build has never heard of is still information, and guessing at its
 * meaning would be worse than showing it.
 */
export function skipReasonText(
  project: BulkActivationJobProjectResponse
): string {
  if (project.skip_detail) return project.skip_detail
  if (project.skip_reason) return project.skip_reason
  return 'The instance recorded no reason for skipping this project.'
}

/**
 * Projects a "Retry" should cover after a job finished with problems.
 *
 * Everything that failed or was skipped, minus the projects that no longer
 * exist — retrying a deleted project would skip identically forever and make
 * the retry button look broken.
 */
export function retryableProjectIds(job: BulkActivationJobResponse): number[] {
  return job.projects
    .filter(
      (project) =>
        (project.status === 'failed' || project.status === 'skipped') &&
        project.skip_reason !== 'project_not_found'
    )
    .map((project) => project.project_id)
}

/**
 * Projects a "Resume" should cover after an aborted or cancelled job.
 *
 * `pending` projects were never touched. `switching`/`backfilling` were
 * in-flight when the job stopped and are resumable from their cursor.
 * `failed` is included because an instance-wide abort marks whatever it
 * interrupted as failed, and that is precisely the work to pick back up.
 */
export function resumableProjectIds(job: BulkActivationJobResponse): number[] {
  return job.projects
    .filter(
      (project) =>
        project.status === 'pending' ||
        project.status === 'switching' ||
        project.status === 'backfilling' ||
        project.status === 'failed'
    )
    .map((project) => project.project_id)
}

// ---------------------------------------------------------------------------
// ETA and progress
// ---------------------------------------------------------------------------

/**
 * A coarse duration. ADR-042 §6: a false-precision countdown that jumps around
 * is worse than an honest range, so nothing finer than a minute is ever shown.
 */
export function coarseDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return 'less than a minute'
  if (seconds < 90) return 'about a minute'
  if (seconds < 3600) return `about ${Math.round(seconds / 60)} minutes`
  if (seconds < 5400) return 'about an hour'
  if (seconds < 86_400) return `about ${Math.round(seconds / 3600)} hours`
  const days = Math.round(seconds / 86_400)
  return days === 1 ? 'about a day' : `about ${days} days`
}

/**
 * The ETA line, branching on `eta_state` rather than on whether a number
 * happens to be present.
 *
 * Returns `null` when there is nothing honest to say — `finished` has no time
 * remaining, and rendering "0 seconds left" would imply otherwise.
 */
export function etaLabel(
  etaState: BulkActivationEtaState,
  etaSeconds: number | null | undefined
): string | null {
  switch (etaState) {
    case 'finished':
      return null
    case 'estimating':
      // Before the first batch acknowledges there is no measured rate. Saying
      // so is the point; inventing a number is what this branch exists to stop.
      return 'estimating…'
    case 'known':
      return typeof etaSeconds === 'number' && Number.isFinite(etaSeconds)
        ? `${coarseDuration(etaSeconds)} left`
        : // `known` without a number should not happen. If it does, the honest
          // answer is still "we are working it out", never a fabricated zero.
          'estimating…'
    default:
      return 'estimating…'
  }
}

/**
 * A percentage, or `—`.
 *
 * The server omits `percent_complete` when the estimate is zero. That is not
 * "0% done" — it is "there is no denominator" — and a stalled 0% bar is exactly
 * the kind of thing an operator debugging alone reads as a hang.
 */
export function percentLabel(percent: number | null | undefined): string {
  return typeof percent === 'number' && Number.isFinite(percent)
    ? `${percent.toFixed(percent < 10 ? 1 : 0)}%`
    : '—'
}

/** The observed rate, coarsely, or `null` when the job has not measured one. */
export function throughputLabel(
  spansPerSec: number | null | undefined
): string | null {
  if (typeof spansPerSec !== 'number' || !Number.isFinite(spansPerSec)) {
    return null
  }
  if (spansPerSec <= 0) return null
  if (spansPerSec < 1) {
    return `${Math.round(spansPerSec * 60).toLocaleString()} spans/min`
  }
  return `${Math.round(spansPerSec).toLocaleString()} spans/s`
}

// ---------------------------------------------------------------------------
// RFC 7807 problem bodies
// ---------------------------------------------------------------------------

/**
 * A named value the server attached to a Problem body.
 *
 * `ErrorBuilder::value(...)` writes these as top-level keys, which is how the
 * 409 carries `batch_id`/`status_path` and the 400 carries `re_estimate_path`.
 */
export function problemValue(error: unknown, key: string): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  const value = (error as Record<string, unknown>)[key]
  if (typeof value === 'string' && value.length > 0) return value
  if (typeof value === 'number') return String(value)
  return undefined
}

/** The HTTP status a Problem body reports, when it reports one. */
export function problemStatus(error: unknown): number | undefined {
  if (!error || typeof error !== 'object') return undefined
  const status = (error as { status?: unknown }).status
  return typeof status === 'number' ? status : undefined
}

// ---------------------------------------------------------------------------
// Server-supplied console paths
// ---------------------------------------------------------------------------

/**
 * Whether a server-supplied path is safe to hand to `<Link to=...>`.
 *
 * `setup_path` is data, and `<Link to="https://elsewhere">` renders an anchor
 * that leaves the console. Only same-document absolute paths are accepted;
 * anything with a scheme, or a protocol-relative `//host`, is refused rather
 * than rendered as a link the operator would reasonably trust.
 */
export function isInternalConsolePath(
  path: string | null | undefined
): path is string {
  if (typeof path !== 'string' || path.length === 0) return false
  // A leading `/` rules out `https:`-style schemes; rejecting `//` and
  // `/\` rules out the protocol-relative forms browsers also treat as
  // absolute.
  if (!path.startsWith('/')) return false
  if (path.startsWith('//') || path.startsWith('/\\')) return false
  // Whitespace and control characters have no business in a console path,
  // and are the usual ingredients of a link that does not read as what it
  // does.
  // eslint-disable-next-line no-control-regex
  if (/[\s\u0000-\u001f]/.test(path)) return false
  return true
}

/**
 * Rewrite `/projects/<id>/...` into `/projects/<slug>/...`.
 *
 * The console routes projects on `:slug` (`/projects/:slug/*` → `GET
 * /projects/by-slug/{slug}`), but the server writes its `setup_path` values
 * with the numeric project id. A numeric id resolves no slug, so the raw path
 * lands on a "project not found" page. Resolving it here — against the project
 * list this card already holds — turns a dead link into a working one without
 * inventing a path the server did not send.
 *
 * Anything that is not that exact shape is returned unchanged: a path this
 * function does not recognise is still the server's to define.
 */
export function resolveConsoleProjectPath(
  path: string,
  slugByProjectId: ReadonlyMap<number, string>
): string {
  const match = /^\/projects\/(\d+)(\/.*)?$/.exec(path)
  if (!match) return path
  const slug = slugByProjectId.get(Number(match[1]))
  if (!slug) return path
  return `/projects/${slug}${match[2] ?? ''}`
}
