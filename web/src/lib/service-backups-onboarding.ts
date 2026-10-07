// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Decisions behind the service page's Backups card and the hand-off to the
// backup schedule form (#1282). Kept free of React so they can be tested.

import type { RestoreRunView } from '@/api/client/types.gen'

/** Path of the form that creates a backup destination (S3 source). */
export const CREATE_BACKUP_DESTINATION_HREF = '/backups/s3-sources/new'

export interface BackupDestinationLike {
  id: number
  is_default?: boolean | null
}

export type BackupsCardState =
  | { kind: 'loading' }
  | { kind: 'no_destination' }
  | { kind: 'empty' }
  | { kind: 'list' }

/**
 * What the Backups card shows. A service without backups gets an onboarding
 * state that names the actual gap: no destination to write to, or simply no
 * backup taken yet.
 */
export function backupsCardState(input: {
  backupsLoading: boolean
  backupCount: number
  destinationsLoading: boolean
  destinations: readonly BackupDestinationLike[] | undefined
}): BackupsCardState {
  if (input.backupsLoading) return { kind: 'loading' }
  if (input.backupCount > 0) return { kind: 'list' }
  // Destinations still loading (or unreadable): don't claim none exist.
  if (input.destinationsLoading || input.destinations === undefined)
    return { kind: 'empty' }
  return input.destinations.length === 0
    ? { kind: 'no_destination' }
    : { kind: 'empty' }
}

/** The destination a new schedule should go to: the default one, else the first. */
export function preferredDestination<T extends BackupDestinationLike>(
  destinations: readonly T[] | undefined
): T | undefined {
  if (!destinations || destinations.length === 0) return undefined
  return destinations.find((d) => d.is_default === true) ?? destinations[0]
}

/**
 * Where "Schedule backups" goes for a service: the schedule form of the
 * preferred destination with the service preselected, or the form that
 * creates a destination when there is none yet.
 */
export function scheduleBackupsHref(
  serviceId: number,
  destinations: readonly BackupDestinationLike[] | undefined
): string {
  const destination = preferredDestination(destinations)
  if (!destination) return CREATE_BACKUP_DESTINATION_HREF
  return `/backups/s3-sources/${destination.id}/schedules/new?service_id=${serviceId}`
}

/** `?service_id=` on the schedule form: the service to preselect, if valid. */
export function preselectedScheduleServiceId(
  params: URLSearchParams
): number | undefined {
  const raw = params.get('service_id')
  if (raw === null || !/^\d+$/.test(raw)) return undefined
  const id = Number(raw)
  return Number.isSafeInteger(id) && id > 0 ? id : undefined
}

/** The restore running on (or into) this service, newest first, if any. */
export function activeRestoreRun(
  runs: readonly RestoreRunView[] | undefined
): RestoreRunView | undefined {
  return runs?.find(
    (run) => run.status === 'pending' || run.status === 'running'
  )
}

/** Anchor of the Backups card on a service's page. */
export const SERVICE_BACKUPS_ANCHOR = 'backups'

/** Link to a service's Backups card. */
export function serviceBackupsHref(serviceId: number): string {
  return `/storage/${serviceId}#${SERVICE_BACKUPS_ANCHOR}`
}

/** Link that follows a restore run on its restore page. */
export function restoreRunHref(serviceId: number, runId: number): string {
  return `/storage/${serviceId}/restore?run=${runId}`
}
