// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ConversationResponse } from '@/api/client'

/**
 * Resolve a durable thread selection after application conversations load.
 * URL state wins. Without it, prefer a thread that has recorded activity;
 * application-thread rows initialize `last_activity_at` to `created_at`, so
 * an untouched test/empty thread must not hide an existing transcript.
 */
export function initialApplicationThreadId(
  conversations: ConversationResponse[],
  requestedId: string | null
): string | null {
  if (
    requestedId &&
    conversations.some((conversation) => conversation.public_id === requestedId)
  ) {
    return requestedId
  }
  return (
    conversations.find(
      (conversation) =>
        conversation.last_activity_at !== conversation.created_at
    )?.public_id ??
    conversations[0]?.public_id ??
    null
  )
}

export type DefaultWorkspaceSelection = {
  applicationId: null
  conversationId: string | null
  openStartScreen: boolean
}

/**
 * Entering the Default workspace is a real scope change even when it has no
 * conversations yet. In that empty case the previous application/thread must
 * be cleared before showing the start screen, otherwise the sidebar and main
 * pane describe two different workspaces and subsequent navigation is stuck
 * behind the stale start screen.
 */
export function defaultWorkspaceSelection(
  conversationIds: string[]
): DefaultWorkspaceSelection {
  const conversationId = conversationIds[0] ?? null
  return {
    applicationId: null,
    conversationId,
    openStartScreen: conversationId == null,
  }
}

/** Keep the current selection unless it was removed; then choose the newest
 * remaining thread. This prevents archiving an open thread from leaving a
 * stale `?thread=` URL that reloads into an empty conversation pane. */
export function threadSelectionAfterRemoval(
  conversationIds: string[],
  currentId: string | null,
  removedId: string
): string | null {
  if (currentId !== removedId) return currentId
  return conversationIds.find((id) => id !== removedId) ?? null
}
