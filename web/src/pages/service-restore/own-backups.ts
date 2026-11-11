// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ServiceBackupListResponse, SourceBackupEntry } from '@/api/client'

/**
 * What to say when no backup is shown: no backups at all, a filter that
 * matches none of them, or nothing compatible on the chosen source.
 */
export function noBackupsMessage(state: {
  fromOwnList: boolean
  total: number
  filter: string
}): string {
  if (state.total > 0 && state.filter.trim() !== '') {
    return 'No backups match this filter.'
  }
  return state.fromOwnList
    ? 'This database has no backups yet.'
    : 'No compatible backups on this source.'
}

/**
 * The restore page normally lists backups per storage source, which needs the
 * list of sources and so a role that may read them. A role that may not (the
 * built-in `user`) can still list and restore its own database's backups, so
 * the page falls back to that list. This turns one of its rows into the entry
 * the page already knows how to show and select.
 *
 * The rows are always tracked backups of this database: they carry no storage
 * format, so a point-in-time restore is not offered from them, and the
 * restore request names the backup by its id, which needs no storage source.
 */
export function ownBackupsAsEntries(
  list: ServiceBackupListResponse,
  service: { name: string; service_type: string }
): SourceBackupEntry[] {
  return list.backups.map((backup) => ({
    backup_id: backup.backup_id,
    backup_type: backup.backup_type,
    created_at: backup.started_at,
    engine: service.service_type,
    expires_at: null,
    format: null,
    id: backup.id,
    location: backup.s3_location,
    metadata_location: '',
    name: backup.name,
    origin_service_name: service.name,
    size_bytes: backup.size_bytes ?? null,
    source: 'db',
    state: backup.state,
  }))
}
