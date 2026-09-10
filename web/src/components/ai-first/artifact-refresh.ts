// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Artifact reads are invalidation-driven. `artifacts_changed` is exact;
 * completion and resync are bounded recovery points for an event missed while
 * the browser was disconnected.
 */
export function shouldRefreshArtifactsForLiveEvent(eventName: string) {
  return (
    eventName === 'artifacts_changed' ||
    eventName === 'turn_complete' ||
    eventName === 'resync_required'
  )
}
