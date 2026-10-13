// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceBackupEntry } from '@/api/client/types.gen'
import { describe, expect, it } from 'bun:test'
import {
  hasBackupSelection,
  isSelectedBackup,
  parseRestoreSelection,
  patchRestoreSelection,
  restoreBackupHref,
} from './restore-selection'

function entry(overrides: Partial<SourceBackupEntry>): SourceBackupEntry {
  return {
    id: 12,
    backup_id: 'uuid-12',
    backup_type: 'full',
    created_at: '2026-10-01T00:00:00Z',
    location: 'external_services/postgres/orders/12',
    metadata_location: '',
    name: 'postgres backup (orders)',
    source: 'db',
    state: 'completed',
    ...overrides,
  } as SourceBackupEntry
}

describe('parseRestoreSelection', () => {
  it('reads source, backup and mode', () => {
    expect(
      parseRestoreSelection(
        new URLSearchParams('source=3&backup=12&mode=new_service')
      )
    ).toEqual({
      sourceId: 3,
      backupId: 12,
      backupLocation: undefined,
      mode: 'new_service',
    })
  })

  it('defaults to an in-place restore with nothing selected', () => {
    expect(parseRestoreSelection(new URLSearchParams(''))).toEqual({
      sourceId: undefined,
      backupId: undefined,
      backupLocation: undefined,
      mode: 'in_place',
    })
  })

  it('ignores malformed values instead of guessing', () => {
    const selection = parseRestoreSelection(
      new URLSearchParams('source=abc&backup=-4&mode=drop_everything')
    )
    expect(selection.sourceId).toBeUndefined()
    expect(selection.backupId).toBeUndefined()
    expect(selection.mode).toBe('in_place')
  })

  it('selects an untracked backup by its location', () => {
    const selection = parseRestoreSelection(
      new URLSearchParams('source=3&location=s3%3A%2F%2Fbucket%2Fbase')
    )
    expect(selection.backupLocation).toBe('s3://bucket/base')
    expect(hasBackupSelection(selection)).toBe(true)
  })
})

describe('patchRestoreSelection', () => {
  it('keeps unrelated parameters such as the followed run', () => {
    const next = patchRestoreSelection(new URLSearchParams('run=9'), {
      sourceId: 3,
      mode: 'pitr',
    })
    expect(next.get('run')).toBe('9')
    expect(next.get('source')).toBe('3')
    expect(next.get('mode')).toBe('pitr')
  })

  it('stores a tracked backup by id and an untracked one by location', () => {
    const tracked = patchRestoreSelection(new URLSearchParams('location=old'), {
      backup: entry({}),
    })
    expect(tracked.get('backup')).toBe('12')
    expect(tracked.has('location')).toBe(false)

    const scanned = patchRestoreSelection(new URLSearchParams('backup=12'), {
      backup: entry({ id: 0, source: 's3_scan', location: 's3://b/base' }),
    })
    expect(scanned.has('backup')).toBe(false)
    expect(scanned.get('location')).toBe('s3://b/base')
  })

  it('clears the backup and omits the default mode', () => {
    const next = patchRestoreSelection(
      new URLSearchParams('source=3&backup=12&mode=pitr'),
      { sourceId: 4, backup: null, mode: 'in_place' }
    )
    expect(next.toString()).toBe('source=4')
  })

  it('round-trips through parseRestoreSelection', () => {
    const next = patchRestoreSelection(new URLSearchParams(''), {
      sourceId: 3,
      backup: entry({}),
      mode: 'new_service',
    })
    const selection = parseRestoreSelection(next)
    expect(isSelectedBackup(entry({}), selection)).toBe(true)
    expect(isSelectedBackup(entry({ id: 13 }), selection)).toBe(false)
    expect(selection.mode).toBe('new_service')
  })
})

describe('selecting a backup from the default source', () => {
  it('pins the source so a saved link survives a change of default', () => {
    // No ?source= yet: the page fell back to the default source (7).
    const next = patchRestoreSelection(new URLSearchParams(), {
      sourceId: 7,
      backup: entry({ id: 12 }),
    })
    const selection = parseRestoreSelection(next)
    expect(selection.sourceId).toBe(7)
    expect(selection.backupId).toBe(12)
  })
})

describe('isSelectedBackup', () => {
  it('never matches an untracked entry by a database id', () => {
    const selection = parseRestoreSelection(new URLSearchParams('backup=12'))
    expect(isSelectedBackup(entry({ source: 's3_scan' }), selection)).toBe(
      false
    )
  })

  it('matches nothing when no backup is selected', () => {
    const selection = parseRestoreSelection(new URLSearchParams('source=3'))
    expect(isSelectedBackup(entry({}), selection)).toBe(false)
    expect(hasBackupSelection(selection)).toBe(false)
  })
})

describe('restoreBackupHref', () => {
  it('links to the restore page with the backup preselected', () => {
    expect(restoreBackupHref(7, { sourceId: 3, backupId: 12 })).toBe(
      '/storage/7/restore?source=3&backup=12'
    )
  })
})
