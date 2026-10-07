// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceBackupEntry } from '@/api/client/types.gen'

/**
 * The restore form's choices live in the URL (`?source=&backup=&mode=`), so a
 * reload, a shared link or a "Restore this backup" link from a backup's page
 * reproduces them instead of starting over.
 *
 * A backup tracked by this instance is identified by its database id
 * (`backup=`); one found only by scanning the bucket has no id and is
 * identified by its storage location (`location=`).
 */

export type RestoreMode = 'in_place' | 'new_service' | 'pitr'

const MODES: readonly RestoreMode[] = ['in_place', 'new_service', 'pitr']

export interface RestoreSelection {
  sourceId?: number
  backupId?: number
  backupLocation?: string
  mode: RestoreMode
}

function positiveInt(value: string | null): number | undefined {
  if (value === null || !/^\d+$/.test(value)) return undefined
  const n = Number(value)
  return Number.isSafeInteger(n) && n > 0 ? n : undefined
}

export function parseRestoreSelection(
  params: URLSearchParams
): RestoreSelection {
  const mode = params.get('mode')
  const backupId = positiveInt(params.get('backup'))
  const location = params.get('location')
  return {
    sourceId: positiveInt(params.get('source')),
    backupId,
    backupLocation: backupId === undefined && location ? location : undefined,
    mode: MODES.includes(mode as RestoreMode)
      ? (mode as RestoreMode)
      : 'in_place',
  }
}

export interface RestoreSelectionPatch {
  sourceId?: number | null
  /** `null` clears the selected backup. */
  backup?: SourceBackupEntry | null
  mode?: RestoreMode
}

/** `prev` with the given choices applied; other parameters are kept. */
export function patchRestoreSelection(
  prev: URLSearchParams,
  patch: RestoreSelectionPatch
): URLSearchParams {
  const next = new URLSearchParams(prev)
  if (patch.sourceId !== undefined) {
    if (patch.sourceId === null) next.delete('source')
    else next.set('source', String(patch.sourceId))
  }
  if (patch.backup !== undefined) {
    next.delete('backup')
    next.delete('location')
    const backup = patch.backup
    if (backup && backup.source === 'db' && backup.id > 0) {
      next.set('backup', String(backup.id))
    } else if (backup?.location) {
      next.set('location', backup.location)
    }
  }
  if (patch.mode !== undefined) {
    if (patch.mode === 'in_place') next.delete('mode')
    else next.set('mode', patch.mode)
  }
  return next
}

/** Whether `entry` is the backup the URL selects. */
export function isSelectedBackup(
  entry: SourceBackupEntry,
  selection: RestoreSelection
): boolean {
  if (selection.backupId !== undefined) {
    return entry.source === 'db' && entry.id === selection.backupId
  }
  if (selection.backupLocation !== undefined) {
    return entry.location === selection.backupLocation
  }
  return false
}

/** Whether the URL names a backup at all. */
export function hasBackupSelection(selection: RestoreSelection): boolean {
  return (
    selection.backupId !== undefined || selection.backupLocation !== undefined
  )
}

/** Link to the restore page for `serviceId` with a backup preselected. */
export function restoreBackupHref(
  serviceId: number,
  backup: { sourceId: number; backupId: number }
): string {
  const params = new URLSearchParams({
    source: String(backup.sourceId),
    backup: String(backup.backupId),
  })
  return `/storage/${serviceId}/restore?${params.toString()}`
}
