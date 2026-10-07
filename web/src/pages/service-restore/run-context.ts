// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  RestoreRunSourceBackup,
  RestoreRunView,
} from '@/api/client/types.gen'
import { isActiveRunStatus, type RunTrackingView } from './restore-state'

// What the running-restore panel says about a run beyond its phase list:
// which backup it reads, and whether it can still be cancelled.

export const RESTORE_NOT_CANCELLABLE_TYPE =
  'https://temps.sh/probs/restore-not-cancellable'
export const RESTORE_NOT_ACTIVE_TYPE =
  'https://temps.sh/probs/restore-not-active'

/**
 * `available`: the server would accept a cancellation now. `past_safe_point`:
 * the run is active but stopping it is no longer safe (or was already
 * requested). `none`: nothing to cancel (finished, or its status is not
 * currently known).
 *
 * The server decides (`cancellable` on the run): an in-place restore stays
 * cancellable while it prepares and downloads the backup, a restore into a
 * new service until that service is registered. The console never re-derives
 * those phases.
 */
export type CancelAvailability = 'available' | 'past_safe_point' | 'none'

export function cancelAvailability(view: RunTrackingView): CancelAvailability {
  if (view.kind !== 'tracking') return 'none'
  if (!isActiveRunStatus(view.run.status)) return 'none'
  return view.run.cancellable ? 'available' : 'past_safe_point'
}

const DEFAULT_NOT_CANCELLABLE =
  'This restore is writing data, so it can no longer be cancelled: stopping it now would leave the database partially restored.'

/** Why an active run cannot be cancelled, in the server's words when it gave them. */
export function notCancellableReason(
  run: Pick<RestoreRunView, 'not_cancellable_reason'>
): string {
  return run.not_cancellable_reason?.trim() || DEFAULT_NOT_CANCELLABLE
}

export interface CancelRefusalCopy {
  level: 'info' | 'error'
  title: string
  description: string
}

/** What to tell the operator when the server refuses a cancellation. */
export function cancelRefusal(error: unknown): CancelRefusalCopy {
  const problem = (error && typeof error === 'object' ? error : {}) as {
    type?: unknown
    detail?: unknown
    message?: unknown
  }
  const detail =
    typeof problem.detail === 'string'
      ? problem.detail
      : typeof problem.message === 'string'
        ? problem.message
        : undefined
  if (problem.type === RESTORE_NOT_CANCELLABLE_TYPE) {
    return {
      level: 'info',
      title: 'This restore can no longer be cancelled',
      description:
        detail ??
        'It has started writing data. Stopping it now would leave the database partially restored, so it will run to completion.',
    }
  }
  if (problem.type === RESTORE_NOT_ACTIVE_TYPE) {
    return {
      level: 'info',
      title: 'This restore already finished',
      description: detail ?? 'There was nothing left to cancel.',
    }
  }
  return {
    level: 'error',
    title: 'Could not cancel the restore',
    description: detail ?? 'The server did not accept the cancellation.',
  }
}

export interface SourceBackupSummary {
  /** Short name for the backup. */
  label: string
  /** Console page of the backup, when it still exists there. */
  href: string | null
  /** Why there is no link, or other context worth a second line. */
  note: string | null
  /** When the backup was taken (ISO 8601), when known. */
  takenAt: string | null
}

/** How to show and link the backup a run restores from. */
export function sourceBackupSummary(
  run: Pick<RestoreRunView, 'source_backup'>
): SourceBackupSummary {
  const backup: RestoreRunSourceBackup | undefined = run.source_backup
  const takenAt = backup?.taken_at ?? null
  if (backup?.backup_id && backup.s3_source_id != null) {
    return {
      label: `Backup #${backup.backup_id.slice(0, 8)}`,
      href: `/backups/s3-sources/${backup.s3_source_id}/backups/${encodeURIComponent(backup.backup_id)}`,
      note: null,
      takenAt,
    }
  }
  if (backup?.id != null) {
    return {
      label: `Backup ${backup.id}`,
      href: null,
      note: 'This backup has since been deleted.',
      takenAt,
    }
  }
  if (backup?.location) {
    return {
      label: backup.location,
      href: null,
      note: 'A backup found in the storage source that this instance did not record.',
      takenAt,
    }
  }
  return { label: 'Unknown backup', href: null, note: null, takenAt }
}

export interface ActiveRestoreCopy {
  /** `warning` while this database's own data is being replaced. */
  tone: 'warning' | 'info'
  title: string
  description: string
}

/** What a database's page says while a restore involving it is running. */
export function activeRestoreCopy(
  run: Pick<RestoreRunView, 'mode' | 'phase' | 'target_service_name'>,
  phaseLabel: string
): ActiveRestoreCopy {
  const newService = run.target_service_name?.trim()
  if (newService) {
    return {
      tone: 'info',
      title: `Restoring a backup into a new database, ${newService}`,
      description: `This database is the template and its data is not changed. Current step: ${phaseLabel}.`,
    }
  }
  return {
    tone: 'warning',
    title: 'A restore is replacing this database’s data',
    description: `Expect the database to be unavailable or to show partial data until it finishes. Current step: ${phaseLabel}.`,
  }
}
