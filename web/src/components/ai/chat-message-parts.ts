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

export interface ChatAttachment {
  id: string
  name: string
  mime_type: string
  size_bytes: number
  sandbox_path: string
  is_image: boolean
  /** Browser-only preview while the selected file remains in memory. */
  preview_url?: string
}

/** One ordered segment of an assistant turn. */
export type ChatPart =
  | { type: 'text'; text: string }
  | { type: 'tool'; tool: ToolCall }
  | { type: 'permission'; permission: PermissionRequest }

/** Local chat message shape mirroring the generated MessageResponse. */
export interface ChatMessage {
  /** Stable opaque cursor assigned to a message persisted by Temps. */
  server_cursor?: string
  role: string
  content: string
  created_at?: string
  tools?: ToolCall[]
  parts?: ChatPart[]
  attachments?: ChatAttachment[]
  /** Ephemeral id used to reconcile an optimistic turn with its WS echo. */
  client_turn_id?: string
}

export interface PendingActionLike {
  public_id: string
  status: string
}

/**
 * Harness MCP clients qualify tool names with their server namespace (for
 * example `mcp__temps-chat__temps_write`). The proposal semantics belong to
 * the final tool segment, not to the transport-specific prefix.
 */
export function isTempsWriteToolName(name: string): boolean {
  return name === 'temps_write' || name === 'mcp__temps-chat__temps_write'
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
      if (part.type !== 'tool' || !isTempsWriteToolName(part.tool.name))
        continue
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

/**
 * Reconcile the durable action list after a recovered card learns its current
 * server status. The parent owns that list; without updating it, a snapshot
 * captured while the action was still proposed keeps rendering a terminal
 * failed/rejected/executed card at the bottom of the transcript forever.
 */
export function reconcilePendingActionStatus<T extends PendingActionLike>(
  actions: T[],
  publicId: string,
  status: string
): T[] {
  let changed = false
  const reconciled = actions.map((action) => {
    if (action.public_id !== publicId || action.status === status) return action
    changed = true
    return { ...action, status }
  })
  return changed ? reconciled : actions
}
