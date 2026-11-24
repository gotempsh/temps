// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SuggestionMode } from '@/api/client'
import type { ChatMessage } from './chat-message-parts'

/**
 * Identifies the answer suggestions would follow on from: the last message, if
 * it is a finished assistant answer. `null` while the user's message is the
 * last one or the answer has no text yet, since there is nothing to suggest
 * from.
 *
 * The key is derived from the answer's text, not its ids: a live answer is
 * replaced by its stored copy after the turn (gaining a server cursor), and a
 * key that changed then would ask, and pay, for suggestions a second time.
 */
export function latestAnswerKey(messages: ChatMessage[]): string | null {
  const last = messages[messages.length - 1]
  const text = last?.role === 'assistant' ? last.content.trim() : ''
  if (!text) return null
  return `answer:${text.length}:${fnv1a(text)}`
}

/** 32-bit FNV-1a, hex. A cache key, not a security boundary. */
function fnv1a(text: string): string {
  let hash = 0x811c9dc5
  for (let i = 0; i < text.length; i++) {
    hash ^= text.charCodeAt(i)
    hash = Math.imul(hash, 0x01000193)
  }
  return (hash >>> 0).toString(16)
}

/**
 * Whether the latest answer arrived after the chat was opened. An answer still
 * being written has no server timestamp yet, and it is new by definition. A
 * clock difference between server and browser only ever turns an automatic
 * suggestion into an offer, never the other way round by more than the skew.
 */
export function answeredSince(
  messages: ChatMessage[],
  openedAt: number
): boolean {
  const last = messages[messages.length - 1]
  if (!last || last.role !== 'assistant') return false
  if (!last.created_at) return true
  const at = Date.parse(last.created_at)
  return Number.isNaN(at) || at >= openedAt
}

/** What the suggestions row under an answer shows. */
export type NextStepsRow = 'hidden' | 'offer' | 'load' | 'saved'

/**
 * - `off`: nothing.
 * - suggestions already saved on the answer: shown as they are, free.
 * - `ask`: offer, and load once the user says yes. Offering is free.
 * - `always`: load, but only for an answer that finished while the chat was
 *   open. Reopening a chat must not spend tokens on an answer the user
 *   already read, so an older answer is offered instead.
 *
 * A "No" for this answer hides the row whatever the mode.
 */
export function nextStepsRow({
  mode,
  accepted,
  dismissed,
  answeredWhileOpen,
  hasSaved,
}: {
  mode: SuggestionMode
  accepted: boolean
  dismissed: boolean
  answeredWhileOpen: boolean
  hasSaved: boolean
}): NextStepsRow {
  if (mode === 'off' || dismissed) return 'hidden'
  if (hasSaved) return 'saved'
  if (accepted) return 'load'
  if (mode === 'always' && answeredWhileOpen) return 'load'
  return 'offer'
}

export const SUGGESTION_MODE_LABELS: Record<SuggestionMode, string> = {
  ask: 'Ask after each answer',
  always: 'Always suggest',
  off: 'Off',
}

/** Short label for the composer control. */
export function suggestionModeSummary(mode: SuggestionMode): string {
  switch (mode) {
    case 'always':
      return 'Suggestions on'
    case 'off':
      return 'Suggestions off'
    case 'ask':
      return 'Ask for suggestions'
  }
}
