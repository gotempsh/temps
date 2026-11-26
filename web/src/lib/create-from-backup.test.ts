// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceBackupEntry } from '@/api/client/types.gen'
import { describe, expect, test } from 'bun:test'
import {
  backupKey,
  createFromBackupRestore,
  enginesCompatible,
  restorableBackups,
  restoreRunHref,
  restoreSelectionHref,
} from './create-from-backup'

function entry(overrides: Partial<SourceBackupEntry>): SourceBackupEntry {
  return {
    backup_id: '',
    backup_type: 'full',
    created_at: '2026-01-01T00:00:00Z',
    engine: 'postgres',
    format: 'walg',
    id: 0,
    location:
      's3://bucket/walg/basebackups_005/base_000000010000000000000002_backup_stop_sentinel.json',
    metadata_location: '',
    name: 'postgres backup',
    source: 's3_scan',
    state: '',
    ...overrides,
  }
}

describe('enginesCompatible', () => {
  test('matches an engine, and object stores with each other', () => {
    expect(enginesCompatible('postgres', 'postgres')).toBe(true)
    expect(enginesCompatible('Postgres', 'postgres')).toBe(true)
    expect(enginesCompatible('rustfs', 's3')).toBe(true)
    expect(enginesCompatible('postgres', 'mariadb')).toBe(false)
    expect(enginesCompatible(null, 'postgres')).toBe(false)
    expect(enginesCompatible('', 'postgres')).toBe(false)
  })
})

describe('restorableBackups', () => {
  test('keeps finished backups of the engine, newest first', () => {
    const older = entry({ id: 1, source: 'db', state: 'completed' })
    const newer = entry({
      created_at: '2026-03-01T00:00:00Z',
      location:
        's3://bucket/walg/basebackups_005/base_000000010000000000000009_backup_stop_sentinel.json',
    })
    const running = entry({
      id: 2,
      source: 'db',
      state: 'running',
      created_at: '2026-04-01T00:00:00Z',
    })
    const otherEngine = entry({ engine: 'mariadb', created_at: '2026-05-01' })
    // An older server's scan row names the whole repository, not a backup.
    const wholeRepository = entry({ location: 's3://bucket/walg/' })
    expect(
      restorableBackups(
        [older, running, otherEngine, newer, wholeRepository],
        'postgres'
      )
    ).toEqual([newer, older])
  })

  test('returns nothing for an engine without backups', () => {
    expect(restorableBackups([entry({})], 'redis')).toEqual([])
  })
})

describe('createFromBackupRestore', () => {
  test('a tracked backup is named by its id, confirmed and in place', () => {
    expect(createFromBackupRestore(entry({ id: 42, source: 'db' }), 7)).toEqual(
      { backup_id: 42, mode: 'in_place', confirm_cross_service: true }
    )
  })

  test('a backup found in S3 is named by its location and source', () => {
    const found = entry({})
    expect(createFromBackupRestore(found, 7)).toEqual({
      backup_location: found.location,
      backup_engine: 'postgres',
      s3_source_id: 7,
      mode: 'in_place',
      confirm_cross_service: true,
    })
  })
})

describe('links', () => {
  test('follow the run, or reopen the restore with the backup selected', () => {
    expect(restoreRunHref(3, 11)).toBe('/storage/3/restore?run=11')
    expect(restoreSelectionHref(3, 7, entry({ id: 42, source: 'db' }))).toBe(
      '/storage/3/restore?source=7&backup=42'
    )
    const found = entry({})
    const href = restoreSelectionHref(3, 7, found)
    expect(new URLSearchParams(href.split('?')[1]).get('location')).toBe(
      found.location
    )
  })

  test('a backup key tells tracked rows from found ones', () => {
    expect(backupKey(entry({ id: 4, source: 'db' }))).toBe('db:4')
    expect(backupKey(entry({ location: 's3://x' }))).toBe('s3:s3://x')
  })
})
