// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface ChatRouteItem {
  public_id: string
}

export const CHAT_COMPOSER_MIN_HEIGHT = 72
export const CHAT_COMPOSER_MAX_HEIGHT = 240

export interface ChatComposerLayout {
  height: number
  overflowY: 'auto' | 'hidden'
}

/**
 * Streaming never locks the draft: a user can type the instruction that
 * interrupts the active turn. Creation state is the only reason to disable it.
 */
export function isChatComposerDisabled(
  hasConversation: boolean,
  starting: boolean,
  lazyCreate: boolean
): boolean {
  return !hasConversation && !starting && !lazyCreate
}

export function chatComposerSubmitAction(
  input: string,
  streaming: boolean
): 'none' | 'send' | 'interrupt-and-send' {
  if (!input.trim()) return 'none'
  return streaming ? 'interrupt-and-send' : 'send'
}

/** Clamp the growing composer while preserving an internal scroll escape hatch. */
export function resolveChatComposerLayout(
  scrollHeight: number
): ChatComposerLayout {
  return {
    height: Math.min(
      Math.max(scrollHeight, CHAT_COMPOSER_MIN_HEIGHT),
      CHAT_COMPOSER_MAX_HEIGHT
    ),
    overflowY: scrollHeight > CHAT_COMPOSER_MAX_HEIGHT ? 'auto' : 'hidden',
  }
}

/**
 * Whether a conversation reported by the chat panel is missing from the
 * already-rendered list. This is the lazy-create transition: the panel knows
 * the new public id immediately, while the sidebar still holds the snapshot
 * fetched before the user sent their first message.
 */
export function conversationListNeedsRefresh<T extends ChatRouteItem>(
  publicId: string | null,
  conversations: T[]
): boolean {
  return (
    publicId !== null &&
    !conversations.some((conversation) => conversation.public_id === publicId)
  )
}

/**
 * Resolve the conversation a full-page chat route should display.
 *
 * A valid URL always wins. Bare and stale URLs fall back to the first item,
 * which is the most recently active conversation returned by the API.
 */
export function resolvePageChat<T extends ChatRouteItem>(
  requestedId: string | null,
  conversations: T[]
): T | null {
  if (requestedId) {
    const requested = conversations.find(
      (conversation) => conversation.public_id === requestedId
    )
    if (requested) return requested
  }

  return conversations[0] ?? null
}

export interface ProjectChatTarget {
  id: number
  slug?: string | null
  name: string
}

export interface ProjectChatState {
  projectId: number
  projectSlug?: string
  projectName: string
  contextType: 'project'
  contextId: string
  title: string
  autoStart: false
}

/** Build the active state for a genuinely new, unsaved project chat. */
export function createProjectChat(
  project: ProjectChatTarget,
  contextId: string
): ProjectChatState {
  return {
    projectId: project.id,
    projectSlug: project.slug ?? undefined,
    projectName: project.name,
    contextType: 'project',
    contextId,
    title: 'Project chat',
    autoStart: false,
  }
}
