// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Optimistic concurrency for `PUT /settings` (#2081).
 *
 * Every settings write in the CLI reads the settings, changes some fields and
 * writes the whole document back. Sending the `revision` it read as
 * `expected_revision` makes the server refuse the write with 409 when someone
 * saved in between, instead of silently reverting their change.
 */

import { getErrorMessage } from './api-client'

/**
 * The PUT body for `current` (the GET response) with `changes` applied, and
 * the revision `current` was read with as the precondition.
 */
export function settingsWriteBody(
  current: object | undefined | null,
  changes: object,
): Record<string, unknown> & { expected_revision?: string } {
  const revision = (current as { revision?: unknown } | null | undefined)?.revision
  return {
    ...(current ?? {}),
    ...changes,
    ...(typeof revision === 'string' ? { expected_revision: revision } : {}),
  }
}

/** Whether a failed write was refused because the settings changed since they were read. */
export function isStaleRevision(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    typeof (error as { current_revision?: unknown }).current_revision === 'string'
  )
}

/** The message for a failed settings write. */
export function settingsWriteError(error: unknown): string {
  if (isStaleRevision(error)) {
    return (
      'The platform settings were changed by someone else while this command ran, ' +
      'so nothing was saved (their change is kept). Run the command again to apply ' +
      'yours on top of it.'
    )
  }
  return getErrorMessage(error)
}
