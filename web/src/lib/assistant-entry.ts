// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where "Ask" goes: the chat the user last had open on the assistant page,
 * or a new one when there is none. The page records the chat it shows; an
 * entry (the sidebar, the top bar) reads it when it is pressed.
 */

import { useEffect } from 'react'
import { useSearchParams } from 'react-router'

export const ASSISTANT_URL = '/chat'

const LAST_CHAT_KEY = 'temps:assistant:last-chat'
/** A conversation's public id, as the assistant page writes it in `?chat=`. */
const CHAT_ID = /^[A-Za-z0-9_-]{1,64}$/

/** Remember the chat open on the assistant page. */
export function rememberLastChat(publicId: string): void {
  if (!CHAT_ID.test(publicId)) return
  try {
    localStorage.setItem(LAST_CHAT_KEY, publicId)
  } catch {
    // Storage is unavailable: Ask opens a new chat.
  }
}

/** The chat last open on the assistant page, if one was recorded. */
export function lastChat(): string | null {
  try {
    const stored = localStorage.getItem(LAST_CHAT_KEY)
    return stored && CHAT_ID.test(stored) ? stored : null
  } catch {
    return null
  }
}

/**
 * The assistant on the last chat, or on a new one. A chat that no longer
 * exists needs no care here: the page falls back to a new chat by itself.
 */
export function assistantHref(): string {
  const chat = lastChat()
  return chat ? `${ASSISTANT_URL}?chat=${chat}` : ASSISTANT_URL
}

/**
 * For the assistant page: keeps on record the chat its address shows. A new
 * chat nothing was sent in has no address yet, so it leaves the last real
 * one on record.
 */
export function useRememberShownChat(): void {
  const [searchParams] = useSearchParams()
  const shown = searchParams.get('chat')
  useEffect(() => {
    if (shown) rememberLastChat(shown)
  }, [shown])
}
