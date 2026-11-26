// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  SourceBackupEntry,
  StartRestoreRequest,
} from '@/api/client/types.gen'
import {
  needsSnapshotKey,
  patchRestoreSelection,
} from '@/pages/service-restore/restore-selection'

/**
 * Creating a database from a backup: the service is created as usual, then
 * the chosen backup is restored into it in place. The new service holds no
 * data yet, so the in-place restore overwrites nothing; it reuses the restore
 * pipeline (preview checks, progress, rollback) instead of a second path.
 */

// S3-compatible object stores share one restore path and restore into each
// other. Mirror of `engines_compatible` in
// crates/temps-backup/src/services/restore.rs.
const OBJECT_STORE_FAMILY = new Set(['s3', 'rustfs', 'minio', 'blob'])

/** Whether a backup made by `backupEngine` restores into a `targetEngine` service. */
export function enginesCompatible(
  backupEngine: string | null | undefined,
  targetEngine: string
): boolean {
  const a = (backupEngine ?? '').toLowerCase()
  const b = targetEngine.toLowerCase()
  if (!a) return false
  if (a === b) return true
  return OBJECT_STORE_FAMILY.has(a) && OBJECT_STORE_FAMILY.has(b)
}

/**
 * The backups a new `engine` service can start from, newest first: the
 * engine's own (or its family's), finished ones only, and no legacy scan row
 * that names a whole WAL-G repository instead of one base backup.
 */
export function restorableBackups(
  backups: SourceBackupEntry[],
  engine: string
): SourceBackupEntry[] {
  return backups
    .filter(
      (backup) =>
        enginesCompatible(backup.engine, engine) &&
        !needsSnapshotKey(backup) &&
        (backup.source !== 'db' || backup.state === 'completed')
    )
    .sort(
      (a, b) =>
        (Date.parse(b.created_at) || 0) - (Date.parse(a.created_at) || 0)
    )
}

/** A stable key for a backup row: its id when tracked, else its location. */
export function backupKey(backup: SourceBackupEntry): string {
  return backup.source === 'db' ? `db:${backup.id}` : `s3:${backup.location}`
}

/**
 * The restore that fills a just-created service with `backup` from storage
 * source `sourceId`. The service is new and empty, so the cross-service
 * confirmation the server asks for an in-place restore is given here; it is
 * recorded in the audit log like any other.
 */
export function createFromBackupRestore(
  backup: SourceBackupEntry,
  sourceId: number
): StartRestoreRequest {
  const selector =
    backup.source === 'db' && backup.id > 0
      ? { backup_id: backup.id }
      : {
          backup_location: backup.location,
          backup_engine: backup.engine ?? null,
          s3_source_id: sourceId,
        }
  return { ...selector, mode: 'in_place', confirm_cross_service: true }
}

/** The restore page of `serviceId`, following run `runId`. */
export function restoreRunHref(serviceId: number, runId: number): string {
  return `/storage/${serviceId}/restore?run=${runId}`
}

/**
 * The restore page of `serviceId` with `backup` from `sourceId` selected, for
 * when the restore could not be started right after creation.
 */
export function restoreSelectionHref(
  serviceId: number,
  sourceId: number,
  backup: SourceBackupEntry
): string {
  const params = patchRestoreSelection(new URLSearchParams(), {
    sourceId,
    backup,
  })
  return `/storage/${serviceId}/restore?${params.toString()}`
}
