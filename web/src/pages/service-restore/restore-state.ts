// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RestoreRunView } from '@/api/client/types.gen'
import { withHttpStatus } from '@/components/nodes/node-eviction'

// Pure state logic for the service restore page. Everything here is
// deterministic and React-free so the page's loading, error and run-tracking
// decisions can be tested without rendering it.

// ----- Query errors ----------------------------------------------------------

/**
 * Why a read failed, as far as the user is concerned:
 * - `forbidden`: 401/403, the session expired or the account lacks access.
 * - `not_found`: 404, the resource no longer exists or the link is wrong.
 * - `unavailable`: 5xx, a network failure, or anything else. The resource may
 *   well exist; the API just could not answer.
 */
export type QueryErrorKind = 'forbidden' | 'not_found' | 'unavailable'

/**
 * Normalise what the generated client hands back on failure into a plain
 * object that carries the HTTP status. The client throws only the Problem
 * body, which has no `status`, and a network failure surfaces as a
 * `TypeError` whose message is not enumerable, so both are flattened here.
 */
export function toQueryError(
  error: unknown,
  status: number | undefined
): Record<string, unknown> {
  if (error instanceof Error) {
    return withHttpStatus(
      {
        title: error.name,
        detail: error.message,
        network: status === undefined,
      },
      status
    )
  }
  return withHttpStatus(error, status)
}

/** The HTTP status a failed read carried, if any. */
export function queryErrorStatus(error: unknown): number | undefined {
  if (!error || typeof error !== 'object') return undefined
  const status = (error as { status?: unknown }).status
  return typeof status === 'number' ? status : undefined
}

const FORBIDDEN_TITLES = new Set([
  'Unauthorized',
  'Authentication Required',
  'Forbidden',
  'Insufficient Permissions',
])

export function classifyQueryError(error: unknown): QueryErrorKind {
  const status = queryErrorStatus(error)
  if (status === 401 || status === 403) return 'forbidden'
  if (status === 404) return 'not_found'
  if (status !== undefined) return 'unavailable'
  // No status: a Problem body thrown by a code path that did not attach one.
  // The title is the only other reliable signal (see the QueryCache handler
  // in App.tsx), so fall back to it before assuming an outage.
  const title =
    error && typeof error === 'object'
      ? (error as { title?: unknown }).title
      : undefined
  if (typeof title === 'string') {
    if (FORBIDDEN_TITLES.has(title)) return 'forbidden'
    if (/not found/i.test(title)) return 'not_found'
  }
  return 'unavailable'
}

/**
 * Retry only reads that might succeed on a second attempt. A 403 or 404 will
 * not change by asking again, and retrying them only delays the explanation
 * the user needs.
 */
export function shouldRetryRead(
  failureCount: number,
  error: unknown,
  maxRetries = 2
): boolean {
  return (
    classifyQueryError(error) === 'unavailable' && failureCount < maxRetries
  )
}

// ----- Page load state (#1238) ---------------------------------------------

/** The slice of a React Query result the load-state helpers need. */
export interface QueryStateLike<T = unknown> {
  status: 'pending' | 'error' | 'success'
  data: T | undefined
  error: unknown
}

export type SectionState =
  | { kind: 'loading' }
  | { kind: 'error'; errorKind: QueryErrorKind }
  | { kind: 'ready' }

/**
 * A query that has data is usable even if its latest refresh failed: a
 * background failure must not replace content with an error. A settled
 * failure without data is an error, never a loading state.
 */
export function sectionState(query: QueryStateLike): SectionState {
  if (query.data !== undefined) return { kind: 'ready' }
  if (query.status === 'error')
    return { kind: 'error', errorKind: classifyQueryError(query.error) }
  return { kind: 'loading' }
}

export type SourcesState =
  | { kind: 'loading' }
  | { kind: 'error'; errorKind: QueryErrorKind }
  | { kind: 'empty' }
  | { kind: 'ready' }

/** A failed source list is an error, never "no sources". */
export function sourcesState(
  query: QueryStateLike<readonly unknown[]>
): SourcesState {
  const section = sectionState(query)
  if (section.kind !== 'ready') return section
  return (query.data?.length ?? 0) === 0 ? { kind: 'empty' } : { kind: 'ready' }
}

/** Copy for a failed read of the target database itself. */
export function serviceLoadErrorCopy(kind: QueryErrorKind): {
  title: string
  description: string
} {
  switch (kind) {
    case 'forbidden':
      return {
        title: 'You do not have access to this database',
        description:
          'Ask an administrator or the project owner for access to this database. If your session expired, sign in again and retry.',
      }
    case 'not_found':
      return {
        title: 'Database not found',
        description: 'This database no longer exists or the link is wrong.',
      }
    case 'unavailable':
      return {
        title: 'Could not load this database',
        description:
          'Could not load this database. Check your connection and retry.',
      }
  }
}

/** Copy for a failed read of a supporting resource (`subject`, lower case). */
export function sectionLoadErrorCopy(
  subject: string,
  kind: QueryErrorKind
): string {
  switch (kind) {
    case 'forbidden':
      return `You do not have permission to read ${subject}. Ask an administrator or the project owner for access to this database.`
    case 'not_found':
      return `Could not find ${subject}. This database may no longer exist or the link is wrong.`
    case 'unavailable':
      return `Could not load ${subject}. Check your connection and retry.`
  }
}

export interface RestoreGate {
  enabled: boolean
  /** Why the restore controls are disabled, when they are. */
  reason?: string
  /** Which read has to be retried to lift the gate. */
  retry?: 'capabilities' | 'active_runs'
}

/**
 * Restore controls stay disabled until the server has said which modes this
 * database supports and that no restore is already running on it. Lost or
 * pending knowledge is never treated as permission to start a destructive
 * operation.
 */
export function restoreGate(
  capabilities: SectionState,
  activeRuns: SectionState
): RestoreGate {
  if (capabilities.kind === 'error')
    return {
      enabled: false,
      reason: sectionLoadErrorCopy(
        'the restore options for this database',
        capabilities.errorKind
      ),
      retry: 'capabilities',
    }
  if (activeRuns.kind === 'error')
    return {
      enabled: false,
      reason: `Could not check whether a restore is already running on this database. ${sectionLoadErrorCopy('its restore history', activeRuns.errorKind)}`,
      retry: 'active_runs',
    }
  if (capabilities.kind === 'loading')
    return {
      enabled: false,
      reason: 'Checking which restore modes this database supports…',
    }
  if (activeRuns.kind === 'loading')
    return {
      enabled: false,
      reason: 'Checking whether a restore is already running on this database…',
    }
  return { enabled: true }
}

// ----- Restore runs (#1237) --------------------------------------------------

export const PHASES: ReadonlyArray<{ id: string; label: string }> = [
  { id: 'prepare', label: 'Prepare' },
  { id: 'provision', label: 'Provision' },
  { id: 'restore', label: 'Restore data' },
  { id: 'recover', label: 'Recover WAL' },
  { id: 'verify', label: 'Verify' },
  { id: 'completed', label: 'Completed' },
]

export function phaseLabel(phase: string): string {
  return PHASES.find((p) => p.id === phase)?.label ?? phase
}

export type TerminalOutcome =
  'completed' | 'failed' | 'cancelled' | 'interrupted'

const TERMINAL_STATUSES: ReadonlySet<string> = new Set<TerminalOutcome>([
  'completed',
  'failed',
  'cancelled',
  'interrupted',
])

export function terminalOutcome(status: string): TerminalOutcome | null {
  return TERMINAL_STATUSES.has(status) ? (status as TerminalOutcome) : null
}

export function isActiveRunStatus(status: string): boolean {
  return status === 'pending' || status === 'running'
}

/** Problem type the server uses for "this service already has a restore running". */
export const RESTORE_ALREADY_ACTIVE_TYPE =
  'https://temps.sh/probs/restore-already-active'

/**
 * `?run=` from the URL: `null` when absent, `'invalid'` when present but not
 * a run id (treated as a run that does not exist), otherwise the id.
 */
export function parseRunParam(raw: string | null): number | 'invalid' | null {
  if (raw === null || raw === '') return null
  if (!/^\d+$/.test(raw)) return 'invalid'
  const id = Number(raw)
  return Number.isSafeInteger(id) && id > 0 ? id : 'invalid'
}

/**
 * The run to reattach to when the URL has none: the newest pending/running
 * run for this service. The server lists runs newest first.
 */
export function pickActiveRun(
  runs: readonly RestoreRunView[] | undefined
): RestoreRunView | undefined {
  return runs?.find((run) => isActiveRunStatus(run.status))
}

/**
 * When a start request was refused because a restore is already running,
 * the run to follow instead. `undefined` means "not that conflict"; `null`
 * means the conflict was reported without a usable run id.
 */
export function activeRestoreConflict(
  error: unknown
): number | null | undefined {
  if (!error || typeof error !== 'object') return undefined
  const problem = error as {
    type?: unknown
    active_restore_run_id?: unknown
    extensions?: { active_restore_run_id?: unknown } | null
  }
  if (problem.type !== RESTORE_ALREADY_ACTIVE_TYPE) return undefined
  const raw =
    problem.active_restore_run_id ?? problem.extensions?.active_restore_run_id
  const id = typeof raw === 'string' ? Number(raw) : raw
  return typeof id === 'number' && Number.isSafeInteger(id) && id > 0
    ? id
    : null
}

/** How this page came to be following a run, when that is worth saying. */
export type AttachReason = 'reattached' | 'already_active'

/**
 * The attach reason is kept in the history entry's state (next to `?run=`),
 * so it survives the URL update and a reload without extra React state.
 */
export interface RestoreLocationState {
  restoreAttach?: AttachReason
}

export function attachReasonFromLocationState(
  state: unknown
): AttachReason | undefined {
  if (!state || typeof state !== 'object') return undefined
  const reason = (state as RestoreLocationState).restoreAttach
  return reason === 'reattached' || reason === 'already_active'
    ? reason
    : undefined
}

export type RunTrackingView =
  | { kind: 'attaching' }
  | { kind: 'tracking'; run: RestoreRunView; confirmedAt: number }
  | {
      kind: 'terminal'
      run: RestoreRunView
      outcome: TerminalOutcome
      confirmedAt: number
    }
  | {
      kind: 'stale' | 'forbidden' | 'not_found'
      lastRun?: RestoreRunView
      confirmedAt?: number
    }

export interface RunQueryLike {
  isError: boolean
  error: unknown
  data: RestoreRunView | undefined
  /** React Query's `dataUpdatedAt`: when `data` was last confirmed (ms). */
  dataUpdatedAt: number
}

/**
 * What the run panel should show. A failed status read never reads as the
 * restore failing or stopping: only a terminal status from the server does.
 */
export function deriveRunTracking(
  runId: number | 'invalid',
  query: RunQueryLike
): RunTrackingView {
  if (runId === 'invalid') return { kind: 'not_found' }
  const run = query.data && query.data.id === runId ? query.data : undefined
  const confirmedAt =
    run && query.dataUpdatedAt > 0 ? query.dataUpdatedAt : undefined
  if (run) {
    const outcome = terminalOutcome(run.status)
    if (outcome)
      return {
        kind: 'terminal',
        run,
        outcome,
        confirmedAt: confirmedAt ?? 0,
      }
  }
  if (query.isError) {
    const errorKind = classifyQueryError(query.error)
    return {
      kind: errorKind === 'unavailable' ? 'stale' : errorKind,
      lastRun: run,
      confirmedAt,
    }
  }
  if (run) return { kind: 'tracking', run, confirmedAt: confirmedAt ?? 0 }
  return { kind: 'attaching' }
}

/**
 * Poll every 2s while a run is active, back off while the API is failing, and
 * stop once the run is terminal or the server says it does not exist. A
 * permission failure stops too: a 401 sends the user to sign in, and the run
 * id in the URL resumes tracking afterwards.
 */
export function runPollInterval(
  data: RestoreRunView | undefined,
  error: unknown,
  isError: boolean
): number | false {
  if (data && terminalOutcome(data.status)) return false
  if (isError) {
    return classifyQueryError(error) === 'unavailable' ? 5000 : false
  }
  return 2000
}

export type TimeFormatter = (epochMs: number) => string

export const formatConfirmedTime: TimeFormatter = (epochMs) =>
  new Date(epochMs).toLocaleTimeString()

/** "Last confirmed phase: Restore data, 14:03:12." or an honest "nothing yet". */
export function lastConfirmedSentence(
  lastRun: RestoreRunView | undefined,
  confirmedAt: number | undefined,
  formatTime: TimeFormatter = formatConfirmedTime
): string {
  if (!lastRun || confirmedAt === undefined)
    return 'No status has been confirmed yet.'
  return `Last confirmed phase: ${phaseLabel(lastRun.phase)}, ${formatTime(confirmedAt)}.`
}

/** Headline and body for a run whose status cannot currently be read. */
export function runTrackingProblemCopy(
  view: Extract<RunTrackingView, { kind: 'stale' | 'forbidden' | 'not_found' }>,
  formatTime: TimeFormatter = formatConfirmedTime
): { title: string; description: string } {
  const last = lastConfirmedSentence(view.lastRun, view.confirmedAt, formatTime)
  switch (view.kind) {
    case 'stale':
      return {
        title: 'Restore status could not be refreshed',
        description: `Restore status could not be refreshed; the restore may still be running. ${last}`,
      }
    case 'forbidden':
      return {
        title: 'Not allowed to read this restore',
        description: `You may need to sign in again, or your account may lack access to this database's restores. The restore may still be running. Signing in again resumes tracking, because the run id is in this page's address. ${last}`,
      }
    case 'not_found':
      return {
        title: 'Restore run not found',
        description:
          'This restore run no longer exists or the run id in the link is wrong.',
      }
  }
}

// ----- Completion feedback ---------------------------------------------------

/**
 * Per-run memory of what the page has seen. `watching` means the run was
 * observed active on this page, so its completion is news worth announcing;
 * `settled` means its outcome has been shown or was already known on arrival.
 */
export type CompletionLedger = Readonly<Record<number, 'watching' | 'settled'>>

/** Record that a run is known to be active (started or reattached here). */
export function markRunWatching(
  ledger: CompletionLedger,
  runId: number
): CompletionLedger {
  if (ledger[runId] !== undefined) return ledger
  return { ...ledger, [runId]: 'watching' }
}

/**
 * Feed every observed run status through here. Completion feedback fires at
 * most once per run, and never for a run that was already terminal the first
 * time this page saw it.
 */
export function observeRun(
  ledger: CompletionLedger,
  run: Pick<RestoreRunView, 'id' | 'status'>
): { ledger: CompletionLedger; notify: TerminalOutcome | null } {
  const seen = ledger[run.id]
  const outcome = terminalOutcome(run.status)
  if (!outcome) {
    return { ledger: markRunWatching(ledger, run.id), notify: null }
  }
  if (seen === 'settled') return { ledger, notify: null }
  return {
    ledger: { ...ledger, [run.id]: 'settled' },
    notify: seen === 'watching' ? outcome : null,
  }
}

// ----- Presentation helpers --------------------------------------------------

export type PhaseState =
  | 'done'
  | 'active'
  | 'last_known'
  | 'failed'
  | 'interrupted'
  | 'stopped'
  | 'pending'

/**
 * Per-phase state for the progress list. `live` is false when the status
 * shown is only the last confirmed one: the current phase is then
 * `last_known`, never an animated "in progress" claim the page cannot back.
 */
export function phaseStates(
  run: Pick<RestoreRunView, 'phase' | 'status'>,
  live: boolean
): Array<{ id: string; label: string; state: PhaseState }> {
  const currentIdx = PHASES.findIndex((p) => p.id === run.phase)
  const outcome = terminalOutcome(run.status)
  return PHASES.map((p, idx) => {
    let state: PhaseState
    if (outcome === 'completed') state = 'done'
    else if (idx < currentIdx) state = 'done'
    else if (idx === currentIdx) {
      if (outcome === 'failed') state = 'failed'
      else if (outcome === 'interrupted') state = 'interrupted'
      else if (outcome === 'cancelled') state = 'stopped'
      else state = live ? 'active' : 'last_known'
    } else state = 'pending'
    return { id: p.id, label: p.label, state }
  })
}

export function interruptedFallbackMessage(phase: string): string {
  return `This restore was interrupted when Temps restarted during ${phase}. The database may be partially restored. Check its health and data before retrying.`
}

export interface CompletionToast {
  level: 'success' | 'error' | 'warning' | 'info'
  title: string
  description: string
}

/** The one-time notification for a run this page watched reach an outcome. */
export function completionToast(
  outcome: TerminalOutcome,
  run: Pick<
    RestoreRunView,
    | 'id'
    | 'phase'
    | 'error_message'
    | 'target_service_id'
    | 'target_service_name'
  >,
  serviceName: string | undefined
): CompletionToast {
  switch (outcome) {
    case 'completed':
      return {
        level: 'success',
        title: 'Restore completed',
        description:
          run.target_service_id != null
            ? `Restored into ${run.target_service_name ?? `service ${run.target_service_id}`}.`
            : `Restored onto ${serviceName ?? 'the database'}.`,
      }
    case 'failed':
      return {
        level: 'error',
        title: 'Restore failed',
        description:
          run.error_message ??
          `Restore run ${run.id} failed without an error message.`,
      }
    case 'cancelled':
      return {
        level: 'info',
        title: 'Restore cancelled',
        description:
          run.error_message ?? `Restore run ${run.id} was cancelled.`,
      }
    case 'interrupted':
      return {
        level: 'warning',
        title: 'Restore interrupted',
        description: run.error_message ?? interruptedFallbackMessage(run.phase),
      }
  }
}
