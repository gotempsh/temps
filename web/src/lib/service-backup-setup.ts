// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { withReturnTo } from '@/lib/same-origin-return-to'

// Links that take an operator from a database's Backups card to the pages
// that configure its backups, and back again once they are done.

/** Anchor of the Backups card on a database's page. */
export const SERVICE_BACKUPS_ANCHOR = 'backups'

/** Where to come back to: the database's Backups card. */
export function serviceBackupsHref(serviceId: number): string {
  return `/storage/${serviceId}#${SERVICE_BACKUPS_ANCHOR}`
}

/** Create a backup destination (S3 source), then return to the database. */
export function createBackupDestinationHref(serviceId: number): string {
  return withReturnTo('/backups/s3-sources/new', serviceBackupsHref(serviceId))
}

/**
 * The new-schedule form on `sourceId`, set to back up only this database,
 * returning to it once the schedule is saved.
 */
export function scheduleServiceBackupsHref(
  sourceId: number,
  serviceId: number
): string {
  return withReturnTo(
    `/backups/s3-sources/${sourceId}/schedules/new?service_id=${serviceId}`,
    serviceBackupsHref(serviceId)
  )
}

/**
 * Whether the Backups card can offer to back this database up.
 *
 * `unknown` covers a caller who may not list destinations (they are global
 * resources, administrators only once projects are confined) or a failed
 * read: the card must not claim there is no destination when it could not
 * check.
 */
export type BackupDestinationState =
  'loading' | 'unknown' | 'none' | 'configured'

export function backupDestinationState(query: {
  isPending: boolean
  isError: boolean
  data?: readonly unknown[] | null
}): BackupDestinationState {
  if (query.isError) return 'unknown'
  if (query.isPending) return 'loading'
  if (!query.data) return 'unknown'
  return query.data.length === 0 ? 'none' : 'configured'
}

/**
 * The `?service_id=` a schedule form was opened with, when it is a valid id.
 */
export function preselectedServiceId(
  params: URLSearchParams
): number | undefined {
  const raw = params.get('service_id')
  if (raw === null || !/^\d+$/.test(raw)) return undefined
  const id = Number(raw)
  return Number.isSafeInteger(id) && id > 0 ? id : undefined
}
