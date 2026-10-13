// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  backupDestinationState,
  createBackupDestinationHref,
  preselectedServiceId,
  scheduleServiceBackupsHref,
  serviceBackupsHref,
} from './service-backup-setup'

function params(href: string): URLSearchParams {
  return new URL(href, 'https://temps.invalid').searchParams
}

describe('service backup setup links', () => {
  it('returns to the Backups card of the database', () => {
    expect(serviceBackupsHref(7)).toBe('/storage/7#backups')
  })

  it('creates a destination and comes back', () => {
    const href = createBackupDestinationHref(7)
    expect(href.startsWith('/backups/s3-sources/new?')).toBe(true)
    expect(params(href).get('returnTo')).toBe('/storage/7#backups')
  })

  it('opens the schedule form for this database only', () => {
    const href = scheduleServiceBackupsHref(3, 7)
    expect(href.startsWith('/backups/s3-sources/3/schedules/new?')).toBe(true)
    expect(params(href).get('service_id')).toBe('7')
    expect(params(href).get('returnTo')).toBe('/storage/7#backups')
  })
})

describe('backupDestinationState', () => {
  it('distinguishes none from unknown', () => {
    expect(
      backupDestinationState({ isPending: false, isError: false, data: [] })
    ).toBe('none')
    expect(
      backupDestinationState({ isPending: false, isError: false, data: [{}] })
    ).toBe('configured')
    expect(backupDestinationState({ isPending: true, isError: false })).toBe(
      'loading'
    )
    // A 403 for a project-scoped caller must not read as "no destination".
    expect(backupDestinationState({ isPending: false, isError: true })).toBe(
      'unknown'
    )
  })
})

describe('preselectedServiceId', () => {
  it('reads a positive integer id only', () => {
    expect(preselectedServiceId(new URLSearchParams('service_id=7'))).toBe(7)
    expect(
      preselectedServiceId(new URLSearchParams('service_id=7abc'))
    ).toBeUndefined()
    expect(
      preselectedServiceId(new URLSearchParams('service_id=0'))
    ).toBeUndefined()
    expect(preselectedServiceId(new URLSearchParams(''))).toBeUndefined()
  })
})
