// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { RestoreRunView } from '@/api/client/types.gen'
import {
  CREATE_BACKUP_DESTINATION_HREF,
  activeRestoreRun,
  backupsCardState,
  preferredDestination,
  preselectedScheduleServiceId,
  restoreRunHref,
  scheduleBackupsHref,
  serviceBackupsHref,
} from './service-backups-onboarding'

const run = (overrides: Partial<RestoreRunView>): RestoreRunView => ({
  id: 1,
  created_at: '2026-01-02T03:00:00Z',
  mode: 'in_place',
  phase: 'prepare',
  source_backup_id: 3,
  source_service_id: 7,
  status: 'running',
  cancellable: true,
  ...overrides,
})

describe('backups card state', () => {
  const base = {
    backupsLoading: false,
    backupCount: 0,
    destinationsLoading: false,
    destinations: [] as { id: number }[],
  }

  test('loading backups shows a skeleton', () => {
    expect(backupsCardState({ ...base, backupsLoading: true })).toEqual({
      kind: 'loading',
    })
  })

  test('no destination is called out instead of a generic empty list', () => {
    expect(backupsCardState(base)).toEqual({ kind: 'no_destination' })
  })

  test('a destination with no backups yet is the plain empty state', () => {
    expect(backupsCardState({ ...base, destinations: [{ id: 2 }] })).toEqual({
      kind: 'empty',
    })
  })

  test('never claims there is no destination while destinations are unknown', () => {
    expect(backupsCardState({ ...base, destinationsLoading: true })).toEqual({
      kind: 'empty',
    })
    expect(backupsCardState({ ...base, destinations: undefined })).toEqual({
      kind: 'empty',
    })
  })

  test('existing backups are listed', () => {
    expect(backupsCardState({ ...base, backupCount: 3 })).toEqual({
      kind: 'list',
    })
  })
})

describe('schedule backups hand-off', () => {
  test('prefers the default destination', () => {
    expect(
      preferredDestination([{ id: 2 }, { id: 5, is_default: true }])?.id
    ).toBe(5)
    expect(preferredDestination([{ id: 2 }, { id: 5 }])?.id).toBe(2)
    expect(preferredDestination([])).toBeUndefined()
  })

  test('opens the schedule form with the service preselected', () => {
    expect(scheduleBackupsHref(7, [{ id: 5, is_default: true }])).toBe(
      '/backups/s3-sources/5/schedules/new?service_id=7'
    )
  })

  test('without a destination, goes to create one first', () => {
    expect(scheduleBackupsHref(7, [])).toBe(CREATE_BACKUP_DESTINATION_HREF)
    expect(scheduleBackupsHref(7, undefined)).toBe(
      CREATE_BACKUP_DESTINATION_HREF
    )
  })

  test('the schedule form reads only a valid service id', () => {
    expect(
      preselectedScheduleServiceId(new URLSearchParams('service_id=7'))
    ).toBe(7)
    expect(
      preselectedScheduleServiceId(new URLSearchParams('service_id=0'))
    ).toBeUndefined()
    expect(
      preselectedScheduleServiceId(new URLSearchParams('service_id=x'))
    ).toBeUndefined()
    expect(
      preselectedScheduleServiceId(new URLSearchParams(''))
    ).toBeUndefined()
  })
})

describe('active restore banner', () => {
  test('finds the first pending or running run', () => {
    expect(
      activeRestoreRun([
        run({ id: 3, status: 'completed' }),
        run({ id: 2, status: 'running' }),
        run({ id: 1, status: 'pending' }),
      ])?.id
    ).toBe(2)
  })

  test('has nothing to show when every run finished', () => {
    expect(
      activeRestoreRun([
        run({ status: 'failed' }),
        run({ status: 'cancelled' }),
      ])
    ).toBeUndefined()
    expect(activeRestoreRun(undefined)).toBeUndefined()
  })

  test('links to a service backups card', () => {
    expect(serviceBackupsHref(7)).toBe('/storage/7#backups')
  })

  test('links to the run on the restore page', () => {
    expect(restoreRunHref(7, 12)).toBe('/storage/7/restore?run=12')
  })
})
