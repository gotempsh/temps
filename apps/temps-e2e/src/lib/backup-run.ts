// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RunExternalServiceBackupRequest } from '@temps-sdk/api'

/**
 * Request body for `POST /backups/external-services/{id}/run` that agrees to
 * the database restart a first WAL-G backup performs.
 *
 * A PostgreSQL service whose WAL archiving is still off is restarted in a new
 * container by its first WAL-G backup. The server refuses that run with
 * `409 RESTART_CONFIRMATION_REQUIRED` unless the caller sends
 * `confirm_restart: true` (the console and CLI ask the operator first). The
 * scenarios provision a fresh service and are the operator here, so they
 * consent explicitly rather than relying on archiving having been enabled.
 *
 * `confirm_restart` is declared locally because the committed `@temps-sdk/api`
 * client predates the field; the generated request type is otherwise reused so
 * the rest of the body stays checked against it.
 */
export type ConfirmedBackupRequest = RunExternalServiceBackupRequest & {
  confirm_restart: true
}

export function confirmedFullBackup(s3SourceId: number): ConfirmedBackupRequest {
  return {
    s3_source_id: s3SourceId,
    backup_type: 'full',
    confirm_restart: true,
  }
}
