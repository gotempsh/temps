// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { ServiceBackupListResponse } from '@/api/client'
import { noBackupsMessage, ownBackupsAsEntries } from './own-backups'

const list: ServiceBackupListResponse = {
  page: 1,
  page_size: 100,
  total: 1,
  backups: [
    {
      backup_id: '5b7c2f0e-0000-4000-8000-000000000001',
      backup_type: 'full',
      compression_type: 'gzip',
      external_service_backup_id: 9,
      id: 41,
      name: 'postgres backup (synthetic-db)',
      s3_location: 'backups/synthetic-db/41',
      s3_source_id: 2,
      s3_source_name: 'synthetic-source',
      size_bytes: 2048,
      started_at: '2026-01-01T10:00:00Z',
      state: 'completed',
    },
  ],
}

test('a database backup becomes a tracked entry that restores by id', () => {
  const [entry] = ownBackupsAsEntries(list, {
    name: 'synthetic-db',
    service_type: 'postgres',
  })
  expect(entry).toMatchObject({
    id: 41,
    source: 'db',
    state: 'completed',
    engine: 'postgres',
    origin_service_name: 'synthetic-db',
    created_at: '2026-01-01T10:00:00Z',
    location: 'backups/synthetic-db/41',
    size_bytes: 2048,
  })
})

test('the entry names no storage format, so a point-in-time restore is not offered', () => {
  const [entry] = ownBackupsAsEntries(list, {
    name: 'synthetic-db',
    service_type: 'postgres',
  })
  expect(entry.format).toBeNull()
})

test('an empty list stays empty', () => {
  expect(
    ownBackupsAsEntries(
      { ...list, backups: [], total: 0 },
      { name: 'synthetic-db', service_type: 'postgres' }
    )
  ).toEqual([])
})

test('says what is actually empty: no backups, a filter that matches none, or nothing on the source', () => {
  expect(noBackupsMessage({ fromOwnList: true, total: 0, filter: '' })).toBe(
    'This database has no backups yet.'
  )
  // Backups exist but the filter hides them all: not "no backups yet".
  expect(noBackupsMessage({ fromOwnList: true, total: 4, filter: 'zzz' })).toBe(
    'No backups match this filter.'
  )
  expect(
    noBackupsMessage({ fromOwnList: false, total: 4, filter: 'zzz' })
  ).toBe('No backups match this filter.')
  expect(noBackupsMessage({ fromOwnList: false, total: 0, filter: '' })).toBe(
    'No compatible backups on this source.'
  )
  // A blank filter is not an active filter.
  expect(noBackupsMessage({ fromOwnList: true, total: 0, filter: '   ' })).toBe(
    'This database has no backups yet.'
  )
})
