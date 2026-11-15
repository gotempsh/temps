// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Whether the full-screen assistant shows its chat list beside the
 * conversation. Hiding it gives the conversation (and its working tabs) the
 * whole width; the list stays one click, or ⌘K, away.
 */
export const CHAT_LIST_STORAGE_KEY = 'temps.assistant.chatList'

/**
 * Where the retired Chats / Work layout choice was kept. `work` hid the chat
 * list, so a stored `work` still means hidden until the user picks again.
 */
const LEGACY_VIEW_STORAGE_KEY = 'temps.assistant.view'

type ViewStorage = Pick<Storage, 'getItem' | 'setItem'>

function defaultStorage(): ViewStorage | null {
  try {
    return typeof window === 'undefined' ? null : window.localStorage
  } catch {
    // Storage can be blocked (private mode); the choice just isn't remembered.
    return null
  }
}

export function readChatListHidden(
  storage: ViewStorage | null = defaultStorage()
): boolean {
  try {
    const stored = storage?.getItem(CHAT_LIST_STORAGE_KEY) ?? null
    if (stored !== null) return stored === 'hidden'
    return storage?.getItem(LEGACY_VIEW_STORAGE_KEY) === 'work'
  } catch {
    return false
  }
}

export function writeChatListHidden(
  hidden: boolean,
  storage: ViewStorage | null = defaultStorage()
): void {
  try {
    storage?.setItem(CHAT_LIST_STORAGE_KEY, hidden ? 'hidden' : 'shown')
  } catch {
    // Not remembered; the choice still applies for this visit.
  }
}

/**
 * Whether a keydown is ⌘B / Ctrl+B, which shows or hides the chat list. It is
 * the key that toggles the console sidebar, which the assistant page replaces.
 */
export function isChatListShortcut(
  event: Pick<
    KeyboardEvent,
    'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'
  >
): boolean {
  return (
    event.key.toLowerCase() === 'b' &&
    (event.metaKey || event.ctrlKey) &&
    !event.altKey &&
    !event.shiftKey
  )
}

/** Whether a keydown is the ⌘K / Ctrl+K shortcut. */
export function isSwitcherShortcut(
  event: Pick<
    KeyboardEvent,
    'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'
  >
): boolean {
  return (
    event.key.toLowerCase() === 'k' &&
    (event.metaKey || event.ctrlKey) &&
    !event.altKey &&
    !event.shiftKey
  )
}

/** What a keydown's target needs to be asked: is this a place you type? */
interface ShortcutTarget {
  tagName?: string
  isContentEditable?: boolean
  closest?: (selector: string) => unknown
}

/** Whether keystrokes aimed at `target` are text being typed, or a dialog's. */
function swallowsTyping(target: unknown): boolean {
  if (!target || typeof target !== 'object') return false
  const element = target as ShortcutTarget
  const tag = element.tagName?.toUpperCase()
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true
  if (element.isContentEditable) return true
  // Focus inside a dialog or menu belongs to it; "/" must not pull it away.
  return Boolean(element.closest?.('[role="dialog"], [role="menu"]'))
}

/**
 * Whether a keydown is a bare "/" that should focus the message box: no
 * ⌘/Ctrl/Alt, not part of an IME composition, not already handled, and not
 * aimed at somewhere you are typing. Shift is allowed because "/" needs it on
 * many keyboard layouts.
 */
export function isFocusComposerShortcut(
  event: Pick<
    KeyboardEvent,
    | 'key'
    | 'metaKey'
    | 'ctrlKey'
    | 'altKey'
    | 'isComposing'
    | 'defaultPrevented'
  > & { target: EventTarget | null }
): boolean {
  return (
    event.key === '/' &&
    !event.metaKey &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.isComposing &&
    !event.defaultPrevented &&
    !swallowsTyping(event.target)
  )
}

/** Appended to a composer's placeholder so the shortcut can be found. */
export const FOCUS_COMPOSER_HINT = ' (press / to focus)'
