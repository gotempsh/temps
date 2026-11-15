// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface ChatRouteItem {
  public_id: string
}

/** Narrow the classic assistant to chats that can use its project routes. */
export function hasProjectScope<T extends { project_id?: number | null }>(
  conversation: T
): conversation is T & { project_id: number } {
  return conversation.project_id != null
}

export const CHAT_COMPOSER_MIN_HEIGHT = 72
/** One line of text: phones have no room for the two-line resting height. */
export const CHAT_COMPOSER_COMPACT_MIN_HEIGHT = 40
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

/**
 * Whether a key press in a composer sends the message. On a phone Enter is a
 * new line (the soft keyboard has no Shift, and the send button is on screen);
 * elsewhere Enter sends and Shift+Enter breaks the line. Enter while an IME is
 * composing only confirms the candidate, so it never sends.
 */
export function enterSendsMessage(
  event: { key: string; shiftKey: boolean; isComposing?: boolean },
  mobile: boolean
): boolean {
  return (
    !mobile &&
    event.key === 'Enter' &&
    !event.shiftKey &&
    event.isComposing !== true
  )
}

/** Clamp the growing composer while preserving an internal scroll escape hatch. */
export function resolveChatComposerLayout(
  scrollHeight: number,
  minHeight: number = CHAT_COMPOSER_MIN_HEIGHT
): ChatComposerLayout {
  return {
    height: Math.min(
      Math.max(scrollHeight, minHeight),
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
 * Find the conversation a full-page chat route (`/chat?chat=<id>`) points at.
 *
 * Only a URL that names an existing conversation resolves. A bare or stale
 * route returns null: the page then opens on the assistant's start screen
 * (onboarding, or "what should we look at?") rather than silently resuming
 * whichever conversation happened to be most recent.
 */
export function resolvePageChat<T extends ChatRouteItem>(
  requestedId: string | null,
  conversations: T[]
): T | null {
  if (!requestedId) return null
  return (
    conversations.find(
      (conversation) => conversation.public_id === requestedId
    ) ?? null
  )
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
    title: 'Service chat',
    autoStart: false,
  }
}

/** How many chats the list asks for at a time. */
export const CHAT_LIST_PAGE_SIZE = 30
/** The most the list endpoint returns in one page. */
const CHAT_LIST_MAX_PAGE_SIZE = 100

export interface ListedChat extends ChatRouteItem {
  last_activity_at: string
}

function activityTime(chat: ListedChat): number {
  const time = Date.parse(chat.last_activity_at)
  return Number.isNaN(time) ? 0 : time
}

/** Most recently active first, keeping the server's order between equals. */
function byActivity<T extends ListedChat>(chats: T[]): T[] {
  return [...chats].sort((a, b) => activityTime(b) - activityTime(a))
}

/**
 * The page to ask for when the user loads older chats, given how many listed
 * chats are already shown. Rounding down re-reads a partly seen page instead
 * of skipping chats that moved up because others were deleted in the
 * meantime; the repeats are dropped by {@link appendChatPage}.
 */
export function nextChatListPage(
  listedCount: number,
  pageSize: number = CHAT_LIST_PAGE_SIZE
): number {
  return Math.floor(listedCount / pageSize) + 1
}

/**
 * How many chats a refresh asks for: enough to cover every page already
 * loaded, so refreshing after a rename does not collapse the list back to the
 * first page.
 */
export function chatListRefreshSize(
  listedCount: number,
  pageSize: number = CHAT_LIST_PAGE_SIZE
): number {
  const pages = Math.max(1, Math.ceil(listedCount / pageSize))
  return Math.min(CHAT_LIST_MAX_PAGE_SIZE, pages * pageSize)
}

/** Adds a page of older chats; the page's copy of a chat already shown wins. */
export function appendChatPage<T extends ListedChat>(
  existing: T[],
  page: T[]
): T[] {
  const fromPage = new Map(page.map((chat) => [chat.public_id, chat]))
  const updated = existing.map((chat) => fromPage.get(chat.public_id) ?? chat)
  const shown = new Set(existing.map((chat) => chat.public_id))
  return byActivity([
    ...updated,
    ...page.filter((chat) => !shown.has(chat.public_id)),
  ])
}

/**
 * Replaces the newest chats with a fresh first page. Older chats the user had
 * already loaded (or opened from a link) stay, unless the fresh page is the
 * whole list: then anything missing from it was deleted.
 */
export function refreshChatList<T extends ListedChat>(
  existing: T[],
  fresh: T[],
  requested: number,
  hadMore: boolean
): { chats: T[]; hasMore: boolean } {
  if (fresh.length < requested) return { chats: fresh, hasMore: false }
  const oldest = Math.min(...fresh.map(activityTime))
  const inFresh = new Set(fresh.map((chat) => chat.public_id))
  const older = existing.filter(
    (chat) => !inFresh.has(chat.public_id) && activityTime(chat) < oldest
  )
  return {
    chats: byActivity([...fresh, ...older]),
    hasMore: older.length > 0 ? hadMore : true,
  }
}

/**
 * Adds a chat opened from a link that is older than the pages loaded so far,
 * so the list shows (and highlights) what is open.
 */
export function withLinkedChat<T extends ListedChat>(
  existing: T[],
  chat: T
): T[] {
  if (existing.some((item) => item.public_id === chat.public_id)) {
    return existing
  }
  return byActivity([...existing, chat])
}

/**
 * Search matches from every chat that the loaded list does not hold. The
 * loaded ones are already offered, filtered as the user types, so they are not
 * repeated.
 */
export function chatsNotListed<T extends { public_id: string }>(
  listed: { public_id: string }[],
  matches: T[]
): T[] {
  const shown = new Set(listed.map((chat) => chat.public_id))
  return matches.filter((chat) => !shown.has(chat.public_id))
}
