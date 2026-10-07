// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { RestoreRunView } from '@/api/client/types.gen'
import {
  RESTORE_ALREADY_ACTIVE_TYPE,
  activeRestoreConflict,
  attachReasonFromLocationState,
  classifyQueryError,
  completionToast,
  deriveRunTracking,
  lastConfirmedSentence,
  markRunWatching,
  observeRun,
  parseRunParam,
  phaseStates,
  pickActiveRun,
  restoreGate,
  runPollInterval,
  runTrackingProblemCopy,
  sectionState,
  serviceLoadErrorCopy,
  shouldRetryRead,
  sourcesState,
  toQueryError,
  type CompletionLedger,
  type QueryStateLike,
  type RunQueryLike,
} from './restore-state'

const T0 = Date.UTC(2026, 0, 2, 3, 4, 5)
const fmt = (ms: number) => new Date(ms).toISOString()

function run(overrides: Partial<RestoreRunView> = {}): RestoreRunView {
  return {
    id: 12,
    created_at: '2026-01-02T03:00:00Z',
    mode: 'in_place',
    phase: 'prepare',
    source_backup_id: 3,
    source_backup: { id: 3 },
    source_service_id: 7,
    status: 'pending',
    ...overrides,
  }
}

const problem = (status: number | undefined, title = 'Problem') =>
  toQueryError({ title, detail: 'details' }, status)
const networkError = toQueryError(new TypeError('Failed to fetch'), undefined)

function runQuery(overrides: Partial<RunQueryLike> = {}): RunQueryLike {
  return {
    isError: false,
    error: null,
    data: undefined,
    dataUpdatedAt: 0,
    ...overrides,
  }
}

describe('query error classification', () => {
  test('401 and 403 are permission failures', () => {
    expect(classifyQueryError(problem(401))).toBe('forbidden')
    expect(classifyQueryError(problem(403))).toBe('forbidden')
  })
  test('404 is not-found', () => {
    expect(classifyQueryError(problem(404))).toBe('not_found')
  })
  test('5xx and network failures are unavailable', () => {
    expect(classifyQueryError(problem(500))).toBe('unavailable')
    expect(classifyQueryError(problem(503))).toBe('unavailable')
    expect(classifyQueryError(networkError)).toBe('unavailable')
  })
  test('the network error keeps its message', () => {
    expect(networkError.detail).toBe('Failed to fetch')
    expect(networkError.status).toBeUndefined()
  })
  test('a status-less Problem falls back to its title', () => {
    expect(classifyQueryError({ title: 'Unauthorized' })).toBe('forbidden')
    expect(classifyQueryError({ title: 'Service Not Found' })).toBe('not_found')
    expect(classifyQueryError({ title: 'Internal Server Error' })).toBe(
      'unavailable'
    )
  })
  test('only outages are retried', () => {
    expect(shouldRetryRead(0, problem(500))).toBe(true)
    expect(shouldRetryRead(0, networkError)).toBe(true)
    expect(shouldRetryRead(2, problem(500))).toBe(false)
    expect(shouldRetryRead(0, problem(403))).toBe(false)
    expect(shouldRetryRead(0, problem(404))).toBe(false)
  })
})

describe('page load state (#1238)', () => {
  const pending: QueryStateLike = {
    status: 'pending',
    data: undefined,
    error: null,
  }
  const failed = (error: unknown): QueryStateLike<never> => ({
    status: 'error',
    data: undefined,
    error,
  })

  test('a settled failure is an error, never loading', () => {
    expect(sectionState(pending)).toEqual({ kind: 'loading' })
    for (const [error, kind] of [
      [problem(403), 'forbidden'],
      [problem(404), 'not_found'],
      [problem(500), 'unavailable'],
      [networkError, 'unavailable'],
    ] as const) {
      expect(sectionState(failed(error))).toEqual({
        kind: 'error',
        errorKind: kind,
      })
    }
  })

  test('a failed background refresh keeps usable data', () => {
    expect(
      sectionState({ status: 'error', data: { id: 1 }, error: problem(500) })
    ).toEqual({ kind: 'ready' })
  })

  test('retry success moves an error to ready', () => {
    expect(sectionState(failed(problem(500))).kind).toBe('error')
    expect(
      sectionState({ status: 'success', data: { id: 1 }, error: null })
    ).toEqual({ kind: 'ready' })
  })

  test('a genuinely empty source list is not a failed one', () => {
    expect(sourcesState({ status: 'success', data: [], error: null })).toEqual({
      kind: 'empty',
    })
    expect(
      sourcesState({ status: 'success', data: [{ id: 1 }], error: null })
    ).toEqual({ kind: 'ready' })
    expect(sourcesState(failed(problem(500)))).toEqual({
      kind: 'error',
      errorKind: 'unavailable',
    })
    expect(sourcesState(failed(networkError)).kind).toBe('error')
  })

  test('service error copy per failure kind', () => {
    expect(serviceLoadErrorCopy('forbidden').description).toContain(
      'Ask an administrator or the project owner for access to this database'
    )
    expect(serviceLoadErrorCopy('not_found').description).toBe(
      'This database no longer exists or the link is wrong.'
    )
    expect(serviceLoadErrorCopy('unavailable').description).toBe(
      'Could not load this database. Check your connection and retry.'
    )
  })

  test('restore controls stay disabled until capabilities and active runs are known', () => {
    const ready = { kind: 'ready' } as const
    const loading = { kind: 'loading' } as const
    expect(restoreGate(ready, ready)).toEqual({ enabled: true })
    expect(restoreGate(loading, ready).enabled).toBe(false)
    expect(restoreGate(loading, ready).reason).toContain(
      'Checking which restore modes'
    )
    expect(restoreGate(ready, loading).enabled).toBe(false)
    const capsFailed = restoreGate(
      { kind: 'error', errorKind: 'forbidden' },
      ready
    )
    expect(capsFailed).toMatchObject({ enabled: false, retry: 'capabilities' })
    expect(capsFailed.reason).toContain('Ask an administrator')
    const runsFailed = restoreGate(ready, {
      kind: 'error',
      errorKind: 'unavailable',
    })
    expect(runsFailed).toMatchObject({ enabled: false, retry: 'active_runs' })
    expect(runsFailed.reason).toContain('already running')
  })
})

describe('run URL and reattach (#1237)', () => {
  test('parses ?run', () => {
    expect(parseRunParam(null)).toBeNull()
    expect(parseRunParam('')).toBeNull()
    expect(parseRunParam('42')).toBe(42)
    expect(parseRunParam('abc')).toBe('invalid')
    expect(parseRunParam('0')).toBe('invalid')
    expect(parseRunParam('-3')).toBe('invalid')
  })

  test('reattaches to the newest pending or running run', () => {
    const runs = [
      run({ id: 9, status: 'failed' }),
      run({ id: 8, status: 'running' }),
      run({ id: 7, status: 'pending' }),
    ]
    expect(pickActiveRun(runs)?.id).toBe(8)
    expect(pickActiveRun([run({ status: 'completed' })])).toBeUndefined()
    expect(
      pickActiveRun([
        run({ status: 'interrupted' }),
        run({ status: 'cancelled' }),
      ])
    ).toBeUndefined()
    expect(pickActiveRun(undefined)).toBeUndefined()
  })

  test('a 409 restore-already-active names the run to follow', () => {
    expect(
      activeRestoreConflict({
        type: RESTORE_ALREADY_ACTIVE_TYPE,
        detail: 'Service 7 already has an active restore (run 42)',
        active_restore_run_id: 42,
      })
    ).toBe(42)
    expect(
      activeRestoreConflict({
        type: RESTORE_ALREADY_ACTIVE_TYPE,
        extensions: { active_restore_run_id: 43 },
      })
    ).toBe(43)
    expect(
      activeRestoreConflict({ type: RESTORE_ALREADY_ACTIVE_TYPE })
    ).toBeNull()
    expect(activeRestoreConflict({ title: 'Validation Error' })).toBeUndefined()
    expect(activeRestoreConflict(undefined)).toBeUndefined()
  })

  test('attach reason round-trips through history state', () => {
    expect(attachReasonFromLocationState({ restoreAttach: 'reattached' })).toBe(
      'reattached'
    )
    expect(
      attachReasonFromLocationState({ restoreAttach: 'already_active' })
    ).toBe('already_active')
    expect(
      attachReasonFromLocationState({ restoreAttach: 'x' })
    ).toBeUndefined()
    expect(attachReasonFromLocationState(null)).toBeUndefined()
  })
})

describe('run tracking view (#1237)', () => {
  test('before the first status read the run is attaching', () => {
    expect(deriveRunTracking(12, runQuery())).toEqual({ kind: 'attaching' })
  })

  test('initial polling failure: no phase confirmed yet, never "failed"', () => {
    const view = deriveRunTracking(
      12,
      runQuery({ isError: true, error: problem(500) })
    )
    expect(view).toEqual({
      kind: 'stale',
      lastRun: undefined,
      confirmedAt: undefined,
    })
    if (view.kind !== 'stale') throw new Error('expected stale')
    const copy = runTrackingProblemCopy(view, fmt)
    expect(copy.description).toBe(
      'Restore status could not be refreshed; the restore may still be running. No status has been confirmed yet.'
    )
    expect(copy.description).not.toMatch(/failed|stopped/i)
  })

  test('failure after known progress keeps the last confirmed phase and time', () => {
    const last = run({ status: 'running', phase: 'restore' })
    const view = deriveRunTracking(
      12,
      runQuery({
        isError: true,
        error: networkError,
        data: last,
        dataUpdatedAt: T0,
      })
    )
    expect(view).toEqual({ kind: 'stale', lastRun: last, confirmedAt: T0 })
    if (view.kind !== 'stale') throw new Error('expected stale')
    expect(runTrackingProblemCopy(view, fmt).description).toBe(
      `Restore status could not be refreshed; the restore may still be running. Last confirmed phase: Restore data, ${fmt(T0)}.`
    )
    // The current phase is shown as last known, not animated as live.
    const states = phaseStates(last, false)
    expect(states.find((p) => p.id === 'restore')?.state).toBe('last_known')
    expect(states.find((p) => p.id === 'prepare')?.state).toBe('done')
    expect(states.find((p) => p.id === 'verify')?.state).toBe('pending')
  })

  test('permission and not-found failures are distinguished', () => {
    const last = run({ status: 'running', phase: 'provision' })
    const forbidden = deriveRunTracking(
      12,
      runQuery({
        isError: true,
        error: problem(403),
        data: last,
        dataUpdatedAt: T0,
      })
    )
    expect(forbidden.kind).toBe('forbidden')
    if (forbidden.kind !== 'forbidden') throw new Error('expected forbidden')
    const copy = runTrackingProblemCopy(forbidden, fmt)
    expect(copy.description).toContain('sign in again')
    expect(copy.description).toContain('Last confirmed phase: Provision')

    const missing = deriveRunTracking(
      12,
      runQuery({ isError: true, error: problem(404) })
    )
    expect(missing.kind).toBe('not_found')
    expect(deriveRunTracking('invalid', runQuery()).kind).toBe('not_found')
  })

  test('retry success resumes live tracking', () => {
    const live = run({ status: 'running', phase: 'verify' })
    expect(
      deriveRunTracking(12, runQuery({ data: live, dataUpdatedAt: T0 }))
    ).toEqual({ kind: 'tracking', run: live, confirmedAt: T0 })
  })

  test('terminal runs recovered on reload render their outcome', () => {
    for (const status of ['completed', 'failed', 'cancelled', 'interrupted']) {
      const r = run({
        status,
        phase: status === 'completed' ? 'completed' : 'restore',
      })
      const view = deriveRunTracking(
        12,
        runQuery({ data: r, dataUpdatedAt: T0 })
      )
      expect(view).toMatchObject({ kind: 'terminal', outcome: status })
    }
  })

  test('interrupted keeps its phase and is not a generic failure', () => {
    const r = run({ status: 'interrupted', phase: 'restore' })
    const states = phaseStates(r, true)
    expect(states.find((p) => p.id === 'restore')?.state).toBe('interrupted')
    expect(states.some((p) => p.state === 'failed')).toBe(false)
    expect(completionToast('interrupted', r, 'db').level).toBe('warning')
    expect(completionToast('interrupted', r, 'db').description).toBe(
      'This restore was interrupted when Temps restarted during restore. The database may be partially restored. Check its health and data before retrying.'
    )
  })

  test('a terminal status wins over a stale read error', () => {
    const r = run({ status: 'completed', phase: 'completed' })
    expect(
      deriveRunTracking(
        12,
        runQuery({
          isError: true,
          error: problem(500),
          data: r,
          dataUpdatedAt: T0,
        })
      ).kind
    ).toBe('terminal')
  })

  test('polling stops on terminal or missing runs and backs off during outages', () => {
    expect(runPollInterval(undefined, null, false)).toBe(2000)
    expect(runPollInterval(run({ status: 'running' }), null, false)).toBe(2000)
    expect(runPollInterval(run({ status: 'interrupted' }), null, false)).toBe(
      false
    )
    expect(
      runPollInterval(run({ status: 'running' }), problem(500), true)
    ).toBe(5000)
    expect(runPollInterval(undefined, networkError, true)).toBe(5000)
    expect(runPollInterval(undefined, problem(404), true)).toBe(false)
    expect(runPollInterval(undefined, problem(403), true)).toBe(false)
  })

  test('last confirmed sentence is honest when nothing was confirmed', () => {
    expect(lastConfirmedSentence(undefined, undefined, fmt)).toBe(
      'No status has been confirmed yet.'
    )
  })
})

describe('completion feedback happens once per run', () => {
  function feed(
    ledger: CompletionLedger,
    statuses: string[],
    id = 12
  ): { ledger: CompletionLedger; notified: string[] } {
    const notified: string[] = []
    for (const status of statuses) {
      const result = observeRun(ledger, { id, status })
      ledger = result.ledger
      if (result.notify) notified.push(result.notify)
    }
    return { ledger, notified }
  }

  test('a run watched to completion notifies exactly once', () => {
    const { notified } = feed({}, [
      'pending',
      'running',
      'running',
      'completed',
      'completed',
      'completed',
    ])
    expect(notified).toEqual(['completed'])
  })

  test('a run started here notifies even if its first read is terminal', () => {
    const { notified } = feed(markRunWatching({}, 12), ['failed', 'failed'])
    expect(notified).toEqual(['failed'])
  })

  test('a run already terminal on arrival never notifies', () => {
    for (const status of ['completed', 'failed', 'interrupted', 'cancelled']) {
      expect(feed({}, [status, status]).notified).toEqual([])
    }
  })

  test('reattaching to a settled run does not re-notify', () => {
    const first = feed({}, ['running', 'interrupted'])
    expect(first.notified).toEqual(['interrupted'])
    const again = feed(markRunWatching(first.ledger, 12), ['interrupted'])
    expect(again.notified).toEqual([])
  })

  test('runs are tracked independently', () => {
    let ledger: CompletionLedger = {}
    ledger = feed(ledger, ['running'], 1).ledger
    const second = feed(ledger, ['completed'], 2)
    expect(second.notified).toEqual([])
    expect(feed(second.ledger, ['completed'], 1).notified).toEqual([
      'completed',
    ])
  })

  test('toast copy per outcome', () => {
    expect(
      completionToast('completed', run({ status: 'completed' }), 'db')
    ).toEqual({
      level: 'success',
      title: 'Restore completed',
      description: 'Restored onto db.',
    })
    expect(
      completionToast(
        'completed',
        run({ target_service_id: 5, target_service_name: 'db-copy' }),
        'db'
      ).description
    ).toBe('Restored into db-copy.')
    expect(
      completionToast('failed', run({ error_message: 'disk full' }), 'db')
    ).toMatchObject({ level: 'error', description: 'disk full' })
  })
})
