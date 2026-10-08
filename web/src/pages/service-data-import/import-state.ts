// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Pure logic for the "Import data" page: what the page shows for a service,
// how runs are labelled, what the form sends, and how API problems are read.
// Kept free of React so it is unit-tested on its own.

import { z } from 'zod'

/** The run fields the page reads (a subset of `DataImportRunResponse`). */
export interface ImportRunLike {
  id: number
  status: string
  phase: string
  target_database: string
  target_object_count?: number | null
  target_size_bytes?: number | null
  error_message?: string | null
  cancel_requested?: boolean
}

/** The availability fields the page reads. */
export interface AvailabilityLike {
  supported: boolean
  available: boolean
  reason?: string | null
}

export type AvailabilityView =
  | { kind: 'loading' }
  | { kind: 'unsupported'; reason: string }
  | { kind: 'unavailable'; reason: string }
  | { kind: 'ready' }

/**
 * What the page renders for a service. A service that cannot receive an
 * import still gets the page, with the reason — never a missing feature.
 */
export function availabilityView(
  availability: AvailabilityLike | undefined
): AvailabilityView {
  if (!availability) return { kind: 'loading' }
  if (!availability.supported) {
    return {
      kind: 'unsupported',
      reason:
        availability.reason ?? 'This service cannot receive imported data.',
    }
  }
  if (!availability.available) {
    return {
      kind: 'unavailable',
      reason: availability.reason ?? 'The service is not ready.',
    }
  }
  return { kind: 'ready' }
}

export function isRunActive(run: Pick<ImportRunLike, 'status'>): boolean {
  return run.status === 'running'
}

export function hasActiveRun(
  runs: ReadonlyArray<Pick<ImportRunLike, 'status'>> | undefined
): boolean {
  return (runs ?? []).some(isRunActive)
}

/** Poll while a run is in flight; stay quiet otherwise. */
export function importPollInterval(
  runs: ReadonlyArray<Pick<ImportRunLike, 'status'>> | undefined
): number | false {
  return hasActiveRun(runs) ? 2000 : false
}

const STATUS_LABELS: Record<string, string> = {
  running: 'Running',
  succeeded: 'Succeeded',
  failed: 'Failed',
  cancelled: 'Cancelled',
  interrupted: 'Interrupted',
}

export function statusLabel(status: string): string {
  return STATUS_LABELS[status] ?? status
}

export type BadgeVariant =
  'default' | 'secondary' | 'destructive' | 'success' | 'warning' | 'outline'

export function statusVariant(status: string): BadgeVariant {
  switch (status) {
    case 'succeeded':
      return 'success'
    case 'failed':
      return 'destructive'
    case 'interrupted':
      return 'warning'
    case 'running':
      return 'secondary'
    default:
      return 'outline'
  }
}

const PHASE_LABELS: Record<string, string> = {
  preparing_target: 'Preparing the target database',
  transferring: 'Copying data',
  verifying: 'Measuring the result',
  finished: 'Finished',
}

export function phaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? phase.replace(/_/g, ' ')
}

/** What a running run is doing right now, in words. */
export function runningLabel(run: ImportRunLike): string {
  if (run.cancel_requested) return 'Cancelling…'
  return `${phaseLabel(run.phase)}…`
}

export function formatBytes(bytes: number | null | undefined): string | null {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) {
    return null
  }
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes / 1024
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`
}

export function pluralize(noun: string, count: number): string {
  return count === 1 ? noun : `${noun}s`
}

/** "12 tables · 4.2 MB" for a successful run, null when nothing was measured. */
export function resultSummary(
  run: ImportRunLike,
  objectNoun: string
): string | null {
  const parts: string[] = []
  if (typeof run.target_object_count === 'number') {
    parts.push(
      `${run.target_object_count} ${pluralize(objectNoun, run.target_object_count)}`
    )
  }
  const size = formatBytes(run.target_size_bytes)
  if (size) parts.push(size)
  return parts.length > 0 ? parts.join(' · ') : null
}

/**
 * Runs that were running in `previous` and have settled in `next` — the
 * ones worth a toast. Runs the page never saw running are ignored, so
 * opening the page does not replay old outcomes.
 */
export function newlySettledRuns<T extends ImportRunLike>(
  previous: ReadonlyArray<ImportRunLike> | undefined,
  next: ReadonlyArray<T> | undefined
): T[] {
  const wasRunning = new Set(
    (previous ?? []).filter(isRunActive).map((run) => run.id)
  )
  return (next ?? []).filter(
    (run) => wasRunning.has(run.id) && !isRunActive(run)
  )
}

/** Databases every server of the engine has; never offered as a target. */
const SYSTEM_DATABASES: Record<string, ReadonlySet<string>> = {
  postgres: new Set(['postgres', 'template0', 'template1']),
  mariadb: new Set([
    'mysql',
    'information_schema',
    'performance_schema',
    'sys',
  ]),
  mongodb: new Set(['admin', 'local', 'config']),
}

/**
 * Existing databases worth suggesting as targets, sorted. None for Redis:
 * its targets are resource names mapped to logical databases, and the
 * explorer lists the logical databases themselves (`db0`, `db1`, …), which
 * are not valid targets.
 */
export function suggestedTargetDatabases(
  serviceType: string,
  names: ReadonlyArray<string> | undefined
): string[] {
  if (serviceType.toLowerCase() === 'redis') return []
  const reserved = SYSTEM_DATABASES[serviceType.toLowerCase()] ?? new Set()
  return Array.from(new Set(names ?? []))
    .filter((name) => !reserved.has(name.toLowerCase()))
    .sort((a, b) => a.localeCompare(b))
}

/**
 * Immediate feedback when the connection string's scheme is not one the
 * engine accepts — the server checks everything, this only saves a
 * round-trip for the most common slip (pasting a URL of another engine).
 */
export function schemeProblem(
  sourceUrl: string,
  schemes: ReadonlyArray<string>
): string | null {
  const match = /^([a-z][a-z0-9+.-]*):\/\//i.exec(sourceUrl.trim())
  if (!match || schemes.length === 0) return null
  const scheme = match[1].toLowerCase()
  if (schemes.includes(scheme)) return null
  return `This service accepts ${schemes
    .map((s) => `${s}://`)
    .join(' or ')} connection strings, not ${scheme}://.`
}

export interface ImportFormValues {
  sourceUrl: string
  targetDatabase: string
  replace: boolean
  confirmTargetDatabase: string
  timeoutMinutes: number
}

export function importFormSchema(
  maxTimeoutMinutes: number,
  maxTargetLength: number = 63
) {
  return z
    .object({
      sourceUrl: z
        .string()
        .trim()
        .min(1, 'Paste the connection string of the database to copy.'),
      targetDatabase: z
        .string()
        .trim()
        .min(1, 'Name the database that receives the data.')
        .max(
          maxTargetLength,
          `Names are at most ${maxTargetLength} characters for this service.`
        ),
      replace: z.boolean(),
      confirmTargetDatabase: z.string(),
      timeoutMinutes: z
        .number({ message: 'Enter a number of minutes.' })
        .int('Use whole minutes.')
        .min(1, 'At least 1 minute.')
        .max(maxTimeoutMinutes, `At most ${maxTimeoutMinutes} minutes.`),
    })
    .refine(
      (values) =>
        !values.replace ||
        values.confirmTargetDatabase.trim() === values.targetDatabase.trim(),
      {
        message: 'Type the database name exactly to confirm replacing it.',
        path: ['confirmTargetDatabase'],
      }
    )
}

/** Body of `POST /external-services/{id}/data-imports`. */
export interface StartImportBody {
  source_url: string
  target_database: string
  replace: boolean
  confirm_target_database?: string | null
  timeout_minutes: number
}

export function buildStartRequest(values: ImportFormValues): StartImportBody {
  const target = values.targetDatabase.trim()
  return {
    source_url: values.sourceUrl.trim(),
    target_database: target,
    replace: values.replace,
    confirm_target_database: values.replace
      ? values.confirmTargetDatabase.trim()
      : null,
    timeout_minutes: values.timeoutMinutes,
  }
}

interface ProblemLike {
  type?: unknown
  error_code?: unknown
  detail?: unknown
  message?: unknown
  active_run_id?: unknown
  extensions?: { error_code?: unknown; active_run_id?: unknown } | null
}

/** The `error_code` of a data import Problem, if the error is one. */
export function problemCode(error: unknown): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  const problem = error as ProblemLike
  const code = problem.error_code ?? problem.extensions?.error_code
  return typeof code === 'string' ? code : undefined
}

/**
 * For a 409 "an import into this database is already running": the id of
 * that run (`null` when the server did not say), `undefined` for any other
 * error.
 */
export function activeImportConflict(
  error: unknown
): number | null | undefined {
  if (problemCode(error) !== 'import-already-running') return undefined
  const problem = error as ProblemLike
  const raw = problem.active_run_id ?? problem.extensions?.active_run_id
  const id = typeof raw === 'string' ? Number(raw) : raw
  return typeof id === 'number' && Number.isSafeInteger(id) && id > 0
    ? id
    : null
}

export function problemDetail(error: unknown): string {
  if (error && typeof error === 'object') {
    const problem = error as ProblemLike
    if (typeof problem.detail === 'string' && problem.detail) {
      return problem.detail
    }
    if (typeof problem.message === 'string' && problem.message) {
      return problem.message
    }
  }
  return 'Unknown error'
}

/** Engine-specific explanation of the target field. */
export function targetDatabaseHint(serviceType: string): string {
  if (serviceType.toLowerCase() === 'redis') {
    return 'The name a project environment uses for this Redis (usually project_environment). Temps gives each name its own logical database, allocating one if the name has none, exactly as when the environment is linked.'
  }
  return 'Created if it does not exist. To give a project environment its data, use the database that environment is linked to (usually project_environment).'
}

/** Engine-specific explanation of the source URL path. */
export function sourcePathHint(serviceType: string): string {
  if (serviceType.toLowerCase() === 'redis') {
    return 'The path picks the logical database to copy (/0 when omitted).'
  }
  return 'The path names the database to copy.'
}

/** Console path of one import run. */
export function importRunPath(serviceId: number, runId: number): string {
  return `/storage/${serviceId}/import-data/${runId}`
}

/** Console path of the import form, with the target pre-filled. */
export function importAgainPath(serviceId: number, target: string): string {
  return `/storage/${serviceId}/import-data?target=${encodeURIComponent(target)}`
}

export type StepState = 'done' | 'current' | 'failed' | 'pending' | 'skipped'

export interface PhaseStep {
  phase: string
  label: string
  state: StepState
}

const STEPS: ReadonlyArray<{ phase: string; label: string }> = [
  { phase: 'preparing_target', label: 'Prepare the target database' },
  { phase: 'transferring', label: 'Copy the data' },
  { phase: 'verifying', label: 'Measure the result' },
]

/**
 * The run's phases as a timeline. A run that did not succeed keeps the phase
 * it stopped in, which is shown as the failed step.
 */
export function phaseSteps(
  run: Pick<ImportRunLike, 'status' | 'phase'>
): PhaseStep[] {
  if (run.status === 'succeeded') {
    return STEPS.map((step) => ({ ...step, state: 'done' }))
  }
  const reached = STEPS.findIndex((step) => step.phase === run.phase)
  const at = reached === -1 ? 0 : reached
  const active = isRunActive(run)
  return STEPS.map((step, index) => {
    let state: StepState
    if (index < at) state = 'done'
    else if (index === at) state = active ? 'current' : 'failed'
    else state = active ? 'pending' : 'skipped'
    return { ...step, state }
  })
}

/** "45s", "3m 12s", "1h 04m" between two instants. */
export function formatDuration(
  startedAt: string,
  finishedAt: string | null | undefined,
  now: number = Date.now()
): string {
  const start = Date.parse(startedAt)
  const end = finishedAt ? Date.parse(finishedAt) : now
  if (!Number.isFinite(start) || !Number.isFinite(end)) return '—'
  const total = Math.max(0, Math.round((end - start) / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, '0')}m`
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, '0')}s`
  return `${seconds}s`
}

/** Headline for a run, by outcome. */
export function runHeadline(run: ImportRunLike, objectNoun: string): string {
  switch (run.status) {
    case 'running':
      return runningLabel(run)
    case 'succeeded': {
      const summary = resultSummary(run, objectNoun)
      return summary ? `Imported ${summary}` : 'Imported'
    }
    case 'failed':
      return 'The import failed'
    case 'cancelled':
      return 'The import was cancelled'
    case 'interrupted':
      return 'The import was interrupted by a server restart'
    default:
      return statusLabel(run.status)
  }
}
