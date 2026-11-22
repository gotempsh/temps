// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What the full-screen assistant keeps in its address, so a link or a reload
 * shows the same screen:
 * - `chat`: the open conversation.
 * - `tab`: the selected working tab (`made`, `preview`, `files`, `changes`,
 *   `terminal`, `project:<id>`, or a deployment as
 *   `<project id>:<deployment id>`).
 * - `panel=open`: the working tabs are open beside the chat.
 */
export const CHAT_PARAM = 'chat'
/**
 * The retired Chats / Work layout. Links copied before it went away still
 * carry it; it is dropped from the address rather than kept as noise.
 */
const LEGACY_VIEW_PARAM = 'view'
export const TAB_PARAM = 'tab'
export const PANEL_PARAM = 'panel'

/**
 * The address as it is right now. A write made from an effect or a callback
 * must start from this, not from the `searchParams` of the render that created
 * it: a child's effect runs before its parent's, so a copy the parent keeps in
 * a ref is still the previous address then, and writing from it undoes the
 * change that caused the render (one click switching the view, the next one
 * finally sticking). `BrowserRouter` updates `window.location` synchronously
 * on every `setSearchParams`.
 */
export function liveSearchParams(): URLSearchParams {
  return new URLSearchParams(window.location.search)
}

/** A copy of `params` with `name` set, or removed when `value` is null/empty. */
export function withParam(
  params: URLSearchParams,
  name: string,
  value: string | null
): URLSearchParams {
  const next = new URLSearchParams(params)
  if (value) next.set(name, value)
  else next.delete(name)
  return next
}

/** The working-tabs state belongs to one conversation; forget it with that chat. */
export function withoutChatScopedParams(
  params: URLSearchParams
): URLSearchParams {
  const next = new URLSearchParams(params)
  next.delete(TAB_PARAM)
  next.delete(PANEL_PARAM)
  return next
}

/** A copy of `params` that names no conversation (and none of its tabs). */
export function withoutChat(params: URLSearchParams): URLSearchParams {
  const next = withoutChatScopedParams(params)
  next.delete(CHAT_PARAM)
  return next
}

/**
 * The address after the open conversation changed, or null when it
 * already says so. One pass, because two writes in the same commit would each
 * start from the same stale address and the second would undo the first.
 */
export function syncedPageParams(
  params: URLSearchParams,
  state: { chat: string | null }
): URLSearchParams | null {
  let next = new URLSearchParams(params)
  let changed = false
  if (state.chat && params.get(CHAT_PARAM) !== state.chat) {
    // Moving from one chat to another must not carry the old chat's tab along;
    // a first chat appearing in a bare address has nothing to drop anyway.
    if (params.has(CHAT_PARAM)) next = withoutChatScopedParams(next)
    next.set(CHAT_PARAM, state.chat)
    changed = true
  }
  if (next.has(LEGACY_VIEW_PARAM)) {
    next.delete(LEGACY_VIEW_PARAM)
    changed = true
  }
  return changed ? next : null
}
