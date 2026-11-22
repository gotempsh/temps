// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

/**
 * How much of the machinery a chat surface shows.
 *
 * `simple` is a chat box on the default model; `advanced` adds the model and
 * reasoning pickers and opens every tool call so its arguments and result are
 * visible without a click. Surfaces that never set a level keep today's
 * behaviour (`DEFAULT_CHAT_DETAIL`).
 */
export type ChatDetailLevel = 'simple' | 'advanced'

export interface ChatDetail {
  /** Show the provider / model / reasoning / permission pickers in the composer. */
  showRuntimeControls: boolean
  /** Render tool-call cards expanded (arguments and result) by default. */
  expandToolCalls: boolean
}

export const DEFAULT_CHAT_DETAIL: ChatDetail = {
  showRuntimeControls: true,
  expandToolCalls: false,
}

export function chatDetailFor(level: ChatDetailLevel | undefined): ChatDetail {
  switch (level) {
    case 'simple':
      return { showRuntimeControls: false, expandToolCalls: false }
    case 'advanced':
      return { showRuntimeControls: true, expandToolCalls: true }
    default:
      return DEFAULT_CHAT_DETAIL
  }
}

export const ChatDetailContext = createContext<ChatDetail>(DEFAULT_CHAT_DETAIL)

export function useChatDetail(): ChatDetail {
  return useContext(ChatDetailContext)
}
