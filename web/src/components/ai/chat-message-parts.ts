// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PermissionRequest } from './PermissionCard'

/** A tool invocation surfaced over the stream or persisted on the message. */
export interface ToolCall {
  id: string
  name: string
  arguments: string
  /** Undefined while running, a string once done; null only from the API. */
  result?: string | null
}

/** One ordered segment of an assistant turn. */
export type ChatPart =
  | { type: 'text'; text: string }
  | { type: 'tool'; tool: ToolCall }
  | { type: 'permission'; permission: PermissionRequest }

/** Local chat message shape mirroring the generated MessageResponse. */
export interface ChatMessage {
  role: string
  content: string
  created_at?: string
  tools?: ToolCall[]
  parts?: ChatPart[]
}

export interface PendingActionLike {
  public_id: string
  status: string
}

/**
 * Render segments for an assistant message, with compatibility for persisted
 * turns whose ordered parts contain tool cards but whose prose lives only in
 * the message content column.
 */
export function assistantParts(message: ChatMessage): ChatPart[] {
  if (message.parts && message.parts.length > 0) {
    if (
      message.content &&
      !message.parts.some((part) => part.type === 'text')
    ) {
      return [...message.parts, { type: 'text', text: message.content }]
    }
    return message.parts
  }

  const parts: ChatPart[] = []
  for (const tool of message.tools ?? []) parts.push({ type: 'tool', tool })
  if (message.content) parts.push({ type: 'text', text: message.content })
  return parts
}

/** Action ids already represented by persisted `temps_write` tool results. */
export function representedPendingActionIds(
  messages: ChatMessage[]
): Set<string> {
  const ids = new Set<string>()
  for (const message of messages) {
    for (const part of assistantParts(message)) {
      if (part.type !== 'tool' || part.tool.name !== 'temps_write') continue
      try {
        const result = JSON.parse(part.tool.result ?? '') as {
          status?: string
          action_id?: unknown
          steps?: Array<{ action_id?: unknown }>
        }
        if (result.status === 'proposed' && result.action_id) {
          ids.add(String(result.action_id))
        }
        if (result.status === 'proposed_plan') {
          for (const step of result.steps ?? []) {
            if (step.action_id) ids.add(String(step.action_id))
          }
        }
      } catch {
        // Help and validation results are plain text, not proposal receipts.
      }
    }
  }
  return ids
}

/**
 * Pending proposals are durable even when linking the tool receipt back to the
 * assistant message fails. Return the proposed actions that need a standalone
 * confirmation card so a reload can never strand an executable proposal.
 */
export function unrepresentedPendingActions<T extends PendingActionLike>(
  messages: ChatMessage[],
  actions: T[]
): T[] {
  const represented = representedPendingActionIds(messages)
  return actions.filter(
    (action) =>
      action.status === 'proposed' && !represented.has(action.public_id)
  )
}
