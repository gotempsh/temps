// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RestoreRunView } from '@/api/client/types.gen'
import { describe, expect, test } from 'bun:test'
import {
  RESTORE_NOT_ACTIVE_TYPE,
  activeRestoreCopy,
  RESTORE_NOT_CANCELLABLE_TYPE,
  cancelAvailability,
  cancelRefusal,
  notCancellableReason,
  sourceBackupSummary,
} from './run-context'
import type { RunTrackingView } from './restore-state'

function run(overrides: Partial<RestoreRunView> = {}): RestoreRunView {
  return {
    id: 9,
    source_backup_id: 12,
    source_service_id: 7,
    mode: 'in_place',
    status: 'running',
    phase: 'prepare',
    cancellable: true,
    created_at: '2026-10-01T00:00:00Z',
    source_backup: {
      id: 12,
      backup_id: '0b7c2f8e-1111-2222-3333-444455556666',
      s3_source_id: 3,
      location: 'external_services/postgres/orders/12',
      taken_at: '2026-10-01T02:00:00Z',
    },
    ...overrides,
  } as RestoreRunView
}

function tracking(r: RestoreRunView): RunTrackingView {
  return { kind: 'tracking', run: r } as RunTrackingView
}

describe('cancelAvailability', () => {
  test('a preparing run can be cancelled', () => {
    expect(cancelAvailability(tracking(run()))).toBe('available')
  })

  test('follows the server, not the phase: a download can still be cancelled', () => {
    expect(
      cancelAvailability(tracking(run({ phase: 'download', cancellable: true })))
    ).toBe('available')
    expect(
      cancelAvailability(
        tracking(
          run({
            target_service_name: 'orders-copy',
            phase: 'provision',
            cancellable: true,
          })
        )
      )
    ).toBe('available')
  })

  test('a run the server will not stop is past its safe point', () => {
    for (const phase of ['restore', 'recover', 'verify']) {
      expect(
        cancelAvailability(tracking(run({ phase, cancellable: false })))
      ).toBe('past_safe_point')
    }
  })

  test('there is nothing to cancel without a live active run', () => {
    expect(cancelAvailability(tracking(run({ status: 'completed' })))).toBe(
      'none'
    )
    expect(cancelAvailability({ kind: 'attaching' } as RunTrackingView)).toBe(
      'none'
    )
    expect(
      cancelAvailability({
        kind: 'terminal',
        outcome: 'cancelled',
        run: run({ status: 'cancelled' }),
      } as RunTrackingView)
    ).toBe('none')
  })
})

describe('cancelRefusal', () => {
  test('explains a run that started writing data', () => {
    const copy = cancelRefusal({ type: RESTORE_NOT_CANCELLABLE_TYPE })
    expect(copy.level).toBe('info')
    expect(copy.description).toContain('partially restored')
  })

  test('shows the server reason a run cannot be stopped', () => {
    const copy = cancelRefusal({
      type: RESTORE_NOT_CANCELLABLE_TYPE,
      detail: 'The new service is being registered (phase \'verify\').',
    })
    expect(copy.description).toContain('being registered')
  })

  test('passes the server detail through for a finished run', () => {
    const copy = cancelRefusal({
      type: RESTORE_NOT_ACTIVE_TYPE,
      detail:
        'Restore run 9 is already completed, so there is nothing to cancel',
    })
    expect(copy.title).toBe('This restore already finished')
    expect(copy.description).toContain('already completed')
  })

  test('reports anything else as an error', () => {
    expect(cancelRefusal(new Error('network down'))).toEqual({
      level: 'error',
      title: 'Could not cancel the restore',
      description: 'network down',
    })
    expect(cancelRefusal(undefined).level).toBe('error')
  })
})

describe('notCancellableReason', () => {
  test('uses the server reason, else explains the write phase', () => {
    expect(
      notCancellableReason({ not_cancellable_reason: 'Already requested.' })
    ).toBe('Already requested.')
    expect(notCancellableReason({ not_cancellable_reason: null })).toContain(
      'partially restored'
    )
  })
})

describe('sourceBackupSummary', () => {
  test('links a tracked backup to its page', () => {
    expect(sourceBackupSummary(run())).toEqual({
      label: 'Backup #0b7c2f8e',
      href: '/backups/s3-sources/3/backups/0b7c2f8e-1111-2222-3333-444455556666',
      note: null,
      takenAt: '2026-10-01T02:00:00Z',
    })
  })

  test('names a deleted backup without a dead link', () => {
    const summary = sourceBackupSummary(
      run({
        source_backup: {
          id: 12,
          backup_id: null,
          s3_source_id: null,
          location: null,
          taken_at: null,
        },
      })
    )
    expect(summary.href).toBeNull()
    expect(summary.label).toBe('Backup 12')
    expect(summary.note).toContain('deleted')
  })

  test('shows the location of an untracked backup', () => {
    const summary = sourceBackupSummary(
      run({
        source_backup_id: 0,
        source_backup: {
          id: null,
          backup_id: null,
          s3_source_id: 3,
          location: 's3://bucket/base_000000010000000000000002',
          taken_at: null,
        },
      })
    )
    expect(summary.href).toBeNull()
    expect(summary.label).toBe('s3://bucket/base_000000010000000000000002')
  })
})

describe('activeRestoreCopy', () => {
  test('warns that an in-place restore is replacing the data', () => {
    const copy = activeRestoreCopy(run({ phase: 'restore' }), 'Restore data')
    expect(copy.tone).toBe('warning')
    expect(copy.title).toContain('replacing this database')
    expect(copy.description).toContain('Restore data')
  })

  test('says a new-service restore leaves this database alone', () => {
    const copy = activeRestoreCopy(
      run({ mode: 'new_service', target_service_name: 'orders-copy' }),
      'Provision'
    )
    expect(copy.tone).toBe('info')
    expect(copy.title).toContain('orders-copy')
    expect(copy.description).toContain('not changed')
  })
})
