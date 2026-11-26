// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Optimistic concurrency for settings forms (#2081).
 *
 * A settings document's GET carries a `revision`; its PUT sends it back as
 * `expected_revision`. When someone else saved in between, the server refuses
 * the write with 409 and a problem carrying `current_revision`, so a stale
 * form never silently reverts their change.
 */

/** Whether a failed save was refused because the settings changed since they were read. */
export function isStaleRevision(error: unknown): boolean {
  return (
    typeof error === 'object' &&
    error !== null &&
    typeof (error as { current_revision?: unknown }).current_revision ===
      'string'
  )
}

/** What a form says when its save was refused as stale. */
export const STALE_REVISION_MESSAGE =
  'Someone else saved these settings after this page loaded, so nothing was saved. Load their values and apply your change again.'
