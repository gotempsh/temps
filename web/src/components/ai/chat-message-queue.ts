// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useSyncExternalStore } from 'react'
import type { ChatAttachment } from './chat-message-parts'

/** A follow-up written while a turn runs, sent once that turn finishes. */
export interface QueuedChatMessage {
  id: string
  text: string
  attachments: ChatAttachment[]
}

type Queue = readonly QueuedChatMessage[]

const EMPTY: Queue = []

/**
 * Queues by conversation, outside React. The chat panel remounts whenever the
 * user switches chats, so component state would silently drop what they
 * queued; here it waits until they come back. It lives for this tab only.
 */
const queues = new Map<string, Queue>()
const listeners = new Set<() => void>()

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/** The messages queued for a conversation, oldest first. */
export function readChatQueue(conversationId: string): Queue {
  return queues.get(conversationId) ?? EMPTY
}

/** Replace a conversation's queue with what `change` makes of it. */
export function updateChatQueue(
  conversationId: string,
  change: (queue: Queue) => Queue
) {
  const next = change(readChatQueue(conversationId))
  if (next.length === 0) queues.delete(conversationId)
  else queues.set(conversationId, next)
  for (const listener of listeners) listener()
}

/** A conversation's queue, and a way to change it. Empty without a chat. */
export function useChatMessageQueue(conversationId: string | null) {
  const queue = useSyncExternalStore(subscribe, () =>
    conversationId ? readChatQueue(conversationId) : EMPTY
  )
  const update = useCallback(
    (change: (queue: Queue) => Queue) => {
      if (conversationId) updateChatQueue(conversationId, change)
    },
    [conversationId]
  )
  return [queue, update] as const
}

/**
 * Everything queued, as the one message it is sent as: the texts in the order
 * they were written, a blank line apart, with all their attachments.
 */
export function mergeQueuedMessages(queue: Queue): {
  text: string
  attachments: ChatAttachment[]
} {
  return {
    text: queue
      .map((message) => message.text.trim())
      .filter(Boolean)
      .join('\n\n'),
    attachments: queue.flatMap((message) => message.attachments),
  }
}

/** Whether a key press asks to queue the draft: ⌘Enter, or Ctrl+Enter. */
export function isQueueShortcut(event: {
  key: string
  metaKey: boolean
  ctrlKey: boolean
  isComposing?: boolean
}): boolean {
  return (
    event.key === 'Enter' &&
    (event.metaKey || event.ctrlKey) &&
    event.isComposing !== true
  )
}

/** How the queue shortcut reads on this computer. */
export function queueShortcutLabel(
  platform: string = typeof navigator === 'undefined' ? '' : navigator.userAgent
): string {
  return /Mac|iPhone|iPad/.test(platform) ? '⌘↵' : 'Ctrl+↵'
}
