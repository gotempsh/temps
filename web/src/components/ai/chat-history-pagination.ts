// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Minimal shape needed to merge durable conversation-history pages. */
export type CursorMessage = {
  server_cursor?: string
}

export type HistoryScrollAnchor = {
  scrollHeight: number
  scrollTop: number
}

export const CHAT_HISTORY_LOAD_THRESHOLD_PX = 96

export function shouldLoadEarlierMessages(
  scrollTop: number,
  hasMore: boolean
): boolean {
  return hasMore && scrollTop <= CHAT_HISTORY_LOAD_THRESHOLD_PX
}

/** Keep the same transcript row under the reader after older rows prepend. */
export function restoredHistoryScrollTop(
  anchor: HistoryScrollAnchor,
  nextScrollHeight: number
): number {
  return Math.max(
    0,
    anchor.scrollTop + Math.max(0, nextScrollHeight - anchor.scrollHeight)
  )
}

/**
 * Prepend an older server page without duplicating messages already recovered
 * by a concurrent live snapshot. Server cursors are stable; optimistic and
 * live-only messages deliberately remain where they are.
 */
export function prependHistoryPage<T extends CursorMessage>(
  current: T[],
  older: T[]
): T[] {
  const known = new Set(
    current.flatMap((message) =>
      message.server_cursor ? [message.server_cursor] : []
    )
  )
  return [
    ...older.filter(
      (message) => !message.server_cursor || !known.has(message.server_cursor)
    ),
    ...current,
  ]
}

/**
 * Reconcile the latest authoritative page while retaining older pages the
 * reader explicitly loaded. Messages without a server cursor are transient
 * optimistic/live state and are replaced by the authoritative snapshot.
 */
export function reconcileLatestHistoryPage<T extends CursorMessage>(
  current: T[],
  latest: T[]
): T[] {
  const latestCursors = new Set(
    latest.flatMap((message) =>
      message.server_cursor ? [message.server_cursor] : []
    )
  )
  const retainedHistory = current.filter(
    (message) =>
      message.server_cursor && !latestCursors.has(message.server_cursor)
  )
  return [...retainedHistory, ...latest]
}
