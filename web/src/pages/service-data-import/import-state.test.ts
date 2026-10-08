// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  activeImportConflict,
  formatDuration,
  importAgainPath,
  importRunPath,
  phaseSteps,
  runHeadline,
  availabilityView,
  buildStartRequest,
  formatBytes,
  hasActiveRun,
  importFormSchema,
  importPollInterval,
  newlySettledRuns,
  phaseLabel,
  problemCode,
  problemDetail,
  resultSummary,
  runningLabel,
  schemeProblem,
  statusVariant,
  sourcePathHint,
  suggestedTargetDatabases,
  targetDatabaseHint,
  type ImportFormValues,
  type ImportRunLike,
} from './import-state'

function run(
  id: number,
  status: string,
  extra: Partial<ImportRunLike> = {}
): ImportRunLike {
  return {
    id,
    status,
    phase: status === 'running' ? 'transferring' : 'finished',
    target_database: 'shop_production',
    ...extra,
  }
}

describe('availabilityView', () => {
  test('is loading until the availability arrives', () => {
    expect(availabilityView(undefined)).toEqual({ kind: 'loading' })
  })

  test('explains an engine that cannot receive imports instead of hiding it', () => {
    expect(
      availabilityView({
        supported: false,
        available: false,
        reason: 'importing data into redis services is not available yet',
      })
    ).toEqual({
      kind: 'unsupported',
      reason: 'importing data into redis services is not available yet',
    })
  })

  test('separates a supported but stopped service from a ready one', () => {
    expect(
      availabilityView({
        supported: true,
        available: false,
        reason: 'the service is stopped; start it before importing data',
      }).kind
    ).toBe('unavailable')
    expect(availabilityView({ supported: true, available: true })).toEqual({
      kind: 'ready',
    })
  })
})

describe('run status', () => {
  test('polls only while a run is in flight', () => {
    expect(importPollInterval([run(1, 'succeeded')])).toBe(false)
    expect(importPollInterval([run(1, 'failed'), run(2, 'running')])).toBe(2000)
    expect(importPollInterval(undefined)).toBe(false)
    expect(hasActiveRun([run(1, 'interrupted')])).toBe(false)
  })

  test('labels phases and a pending cancellation', () => {
    expect(phaseLabel('transferring')).toBe('Copying data')
    expect(phaseLabel('something_new')).toBe('something new')
    expect(runningLabel(run(1, 'running'))).toBe('Copying data…')
    expect(runningLabel(run(1, 'running', { cancel_requested: true }))).toBe(
      'Cancelling…'
    )
  })

  test('colours outcomes by severity', () => {
    expect(statusVariant('succeeded')).toBe('success')
    expect(statusVariant('failed')).toBe('destructive')
    expect(statusVariant('interrupted')).toBe('warning')
    expect(statusVariant('cancelled')).toBe('outline')
  })

  test('reports only runs this page saw running as newly settled', () => {
    const previous = [run(1, 'running'), run(2, 'succeeded')]
    const next = [run(1, 'failed'), run(2, 'succeeded'), run(3, 'failed')]
    expect(newlySettledRuns(previous, next).map((r) => r.id)).toEqual([1])
    expect(newlySettledRuns(undefined, next)).toEqual([])
  })
})

describe('result summary', () => {
  test('formats sizes', () => {
    expect(formatBytes(null)).toBeNull()
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(50 * 1024 * 1024)).toBe('50 MB')
  })

  test('combines object count and size with the engine noun', () => {
    expect(
      resultSummary(
        run(1, 'succeeded', {
          target_object_count: 1,
          target_size_bytes: 8192,
        }),
        'collection'
      )
    ).toBe('1 collection · 8.0 KB')
    expect(
      resultSummary(run(1, 'succeeded', { target_object_count: 12 }), 'table')
    ).toBe('12 tables')
    expect(resultSummary(run(1, 'succeeded'), 'table')).toBeNull()
  })
})

describe('target suggestions', () => {
  test('drop the engine system databases and sort', () => {
    expect(
      suggestedTargetDatabases('postgres', [
        'template1',
        'shop_production',
        'postgres',
        'blog_staging',
        'shop_production',
      ])
    ).toEqual(['blog_staging', 'shop_production'])
    expect(
      suggestedTargetDatabases('mongodb', ['admin', 'local', 'shop'])
    ).toEqual(['shop'])
  })
})

describe('schemeProblem', () => {
  test('flags a connection string of another engine', () => {
    expect(
      schemeProblem('mysql://u:p@db.example.com/app', [
        'postgres',
        'postgresql',
      ])
    ).toBe(
      'This service accepts postgres:// or postgresql:// connection strings, not mysql://.'
    )
    expect(
      schemeProblem('postgresql://u:p@db.example.com/app', [
        'postgres',
        'postgresql',
      ])
    ).toBeNull()
    expect(schemeProblem('not a url yet', ['postgres'])).toBeNull()
  })
})

describe('form', () => {
  const values: ImportFormValues = {
    sourceUrl: ' postgres://u:p@db.example.com/shop ',
    targetDatabase: ' shop_production ',
    replace: false,
    confirmTargetDatabase: '',
    timeoutMinutes: 60,
  }

  test('requires repeating the database name to replace it', () => {
    const schema = importFormSchema(1440)
    expect(schema.safeParse(values).success).toBe(true)
    expect(schema.safeParse({ ...values, replace: true }).success).toBe(false)
    expect(
      schema.safeParse({
        ...values,
        replace: true,
        confirmTargetDatabase: 'shop_production',
      }).success
    ).toBe(true)
  })

  test('bounds the timeout', () => {
    const schema = importFormSchema(120)
    expect(schema.safeParse({ ...values, timeoutMinutes: 0 }).success).toBe(
      false
    )
    expect(schema.safeParse({ ...values, timeoutMinutes: 121 }).success).toBe(
      false
    )
  })

  test('sends trimmed values and the confirmation only when replacing', () => {
    expect(buildStartRequest(values)).toEqual({
      source_url: 'postgres://u:p@db.example.com/shop',
      target_database: 'shop_production',
      replace: false,
      confirm_target_database: null,
      timeout_minutes: 60,
    })
    expect(
      buildStartRequest({
        ...values,
        replace: true,
        confirmTargetDatabase: 'shop_production',
      }).confirm_target_database
    ).toBe('shop_production')
  })
})

describe('problems', () => {
  test('reads the running import a 409 points at', () => {
    expect(
      activeImportConflict({
        error_code: 'import-already-running',
        active_run_id: 7,
      })
    ).toBe(7)
    expect(
      activeImportConflict({
        extensions: { error_code: 'import-already-running' },
      })
    ).toBeNull()
    expect(
      activeImportConflict({ error_code: 'target-not-empty' })
    ).toBeUndefined()
    expect(activeImportConflict(new Error('network'))).toBeUndefined()
  })

  test('reads codes and details defensively', () => {
    expect(problemCode({ error_code: 'target-not-empty' })).toBe(
      'target-not-empty'
    )
    expect(problemCode('boom')).toBeUndefined()
    expect(problemDetail({ detail: 'Database is not empty' })).toBe(
      'Database is not empty'
    )
    expect(problemDetail(new Error('offline'))).toBe('offline')
    expect(problemDetail(null)).toBe('Unknown error')
  })
})

describe('redis', () => {
  test('never suggests explorer logical databases as targets', () => {
    expect(suggestedTargetDatabases('redis', ['db0', 'db1'])).toEqual([])
  })

  test('explains resource names and the optional source path', () => {
    expect(targetDatabaseHint('redis')).toContain('logical database')
    expect(targetDatabaseHint('postgres')).toContain(
      'Created if it does not exist'
    )
    expect(sourcePathHint('redis')).toContain('/0 when omitted')
    expect(sourcePathHint('mariadb')).toBe(
      'The path names the database to copy.'
    )
  })
})

describe('run detail', () => {
  test('builds paths, escaping the target', () => {
    expect(importRunPath(4, 31)).toBe('/storage/4/import-data/31')
    expect(importAgainPath(4, 'shop prod')).toBe(
      '/storage/4/import-data?target=shop%20prod'
    )
  })

  test('shows a running run at its current phase', () => {
    expect(
      phaseSteps({ status: 'running', phase: 'transferring' }).map(
        (s) => s.state
      )
    ).toEqual(['done', 'current', 'pending'])
  })

  test('marks where an unsuccessful run stopped', () => {
    expect(
      phaseSteps({ status: 'failed', phase: 'transferring' }).map(
        (s) => s.state
      )
    ).toEqual(['done', 'failed', 'skipped'])
    expect(
      phaseSteps({ status: 'interrupted', phase: 'preparing_target' }).map(
        (s) => s.state
      )
    ).toEqual(['failed', 'skipped', 'skipped'])
  })

  test('completes every step on success', () => {
    expect(
      phaseSteps({ status: 'succeeded', phase: 'finished' }).every(
        (s) => s.state === 'done'
      )
    ).toBe(true)
  })

  test('formats durations', () => {
    const start = '2026-10-08T10:00:00Z'
    expect(formatDuration(start, '2026-10-08T10:00:45Z')).toBe('45s')
    expect(formatDuration(start, '2026-10-08T10:03:07Z')).toBe('3m 07s')
    expect(formatDuration(start, '2026-10-08T11:04:00Z')).toBe('1h 04m')
    expect(
      formatDuration(start, null, Date.parse('2026-10-08T10:00:10Z'))
    ).toBe('10s')
    expect(formatDuration('not a date', null)).toBe('—')
  })

  test('summarises each outcome', () => {
    expect(
      runHeadline(run(1, 'succeeded', { target_object_count: 3 }), 'table')
    ).toBe('Imported 3 tables')
    expect(runHeadline(run(1, 'interrupted'), 'table')).toContain('restart')
    expect(runHeadline(run(1, 'running'), 'table')).toBe('Copying data…')
  })
})
