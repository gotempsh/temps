// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { scheduleServiceBackupsHref } from './service-backup-setup'

/** Only an explicitly selected destination opens schedule setup after creation. */
export function creationBackupHref(
  serviceId: number,
  destination: string
): string {
  const sourceId = Number(destination)
  return Number.isSafeInteger(sourceId) && sourceId > 0
    ? scheduleServiceBackupsHref(sourceId, serviceId)
    : `/storage/${serviceId}`
}
