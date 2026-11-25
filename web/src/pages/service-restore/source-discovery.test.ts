// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { SourceBackupEntry } from '@/api/client'
import { mergeDiscovery, visibleOrphans } from './source-discovery'

function entry(location: string, source = 's3_scan'): SourceBackupEntry {
  return {
    id: source === 'db' ? 1 : 0,
    name: 'service-a',
    backup_id: source === 'db' ? 'backup-a' : '',
    backup_type: 'full',
    created_at: '2026-01-01T00:00:00Z',
    location,
    metadata_location: '',
    source,
    state: 'completed',
  }
}

test('incomplete discovery keeps only missing previous rows stale until a complete scan', () => {
  const a = entry('snapshot-a')
  const b = entry('snapshot-b')
  const c = entry('snapshot-c')
  const index = { backups: [a, b], scan_errors: [], last_updated: a.created_at }
  const first = mergeDiscovery(undefined, index)
  const partial = mergeDiscovery(first, {
    ...index,
    backups: [b, c],
    scan_errors: ['Access denied'],
  })
  expect(partial.orphans).toEqual([b, c, a])
  expect([...partial.staleLocations]).toEqual([a.location])
  expect(partial.errors).toEqual(['Access denied'])
  expect(visibleOrphans([entry(a.location, 'db')], partial)).toEqual([b, c])
  expect(mergeDiscovery(partial, { ...index, backups: [] }).orphans).toEqual([])
})
