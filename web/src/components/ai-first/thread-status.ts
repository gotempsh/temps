// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ThreadDisplayStatus = 'pending' | 'error' | 'succeeded'

/**
 * Collapse the server-owned turn lifecycle into the three states users need
 * while scanning the thread switcher. New/idle threads remain pending until a
 * first turn succeeds; cancelled turns are grouped with unsuccessful turns.
 */
export function threadDisplayStatus(
  turnStatus: string | null | undefined,
  hasRecordedActivity = false
): ThreadDisplayStatus {
  if (turnStatus === 'completed') return 'succeeded'
  if (turnStatus === 'failed' || turnStatus === 'cancelled') return 'error'
  // Rows created before durable terminal turn state was introduced retain the
  // old `idle` value. Recorded activity proves those are completed threads,
  // while a brand-new untouched row remains pending.
  if (turnStatus === 'idle' && hasRecordedActivity) return 'succeeded'
  return 'pending'
}
