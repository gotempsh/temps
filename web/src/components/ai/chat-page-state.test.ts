// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  appendChatPage,
  chatComposerSubmitAction,
  chatListRefreshSize,
  chatsNotListed,
  conversationListNeedsRefresh,
  createProjectChat,
  hasProjectScope,
  isChatComposerDisabled,
  CHAT_COMPOSER_COMPACT_MIN_HEIGHT,
  enterSendsMessage,
  nextChatListPage,
  refreshChatList,
  resolveChatComposerLayout,
  resolvePageChat,
  withLinkedChat,
} from './chat-page-state'

describe('hasProjectScope', () => {
  test('keeps classic project chats and excludes user-owned application chats', () => {
    const items = [
      { public_id: 'project', project_id: 42 },
      { public_id: 'application', project_id: null },
    ]

    expect(items.filter(hasProjectScope)).toEqual([
      { public_id: 'project', project_id: 42 },
    ])
  })
})

const conversations = [
  { public_id: 'most-recent' },
  { public_id: 'older-chat' },
]

describe('resolvePageChat', () => {
  test('keeps the conversation selected by the URL across reloads', () => {
    expect(resolvePageChat('older-chat', conversations)?.public_id).toBe(
      'older-chat'
    )
  })

  test('does not resume a conversation on a bare chat route', () => {
    expect(resolvePageChat(null, conversations)).toBeNull()
  })

  test('does not substitute another conversation for a stale URL', () => {
    expect(resolvePageChat('deleted-chat', conversations)).toBeNull()
  })

  test('is empty when there are no conversations', () => {
    expect(resolvePageChat('older-chat', [])).toBeNull()
  })
})

describe('createProjectChat', () => {
  test('creates a fresh active chat instead of opening the project picker', () => {
    expect(
      createProjectChat(
        { id: 42, slug: 'example-project', name: 'Example project' },
        'fresh-context-id'
      )
    ).toEqual({
      projectId: 42,
      projectSlug: 'example-project',
      projectName: 'Example project',
      contextType: 'project',
      contextId: 'fresh-context-id',
      title: 'Service chat',
      autoStart: false,
    })
  })
})

describe('conversationListNeedsRefresh', () => {
  test('refreshes the sidebar when the first message creates a new chat', () => {
    expect(conversationListNeedsRefresh('new-chat', conversations)).toBe(true)
  })

  test('does not refetch for a conversation already shown in the sidebar', () => {
    expect(conversationListNeedsRefresh('older-chat', conversations)).toBe(
      false
    )
  })

  test('does not refetch while the new chat is still an unsaved draft', () => {
    expect(conversationListNeedsRefresh(null, conversations)).toBe(false)
  })
})

describe('resolveChatComposerLayout', () => {
  test('keeps a short draft at the compact minimum height', () => {
    expect(resolveChatComposerLayout(28)).toEqual({
      height: 72,
      overflowY: 'hidden',
    })
  })

  test('rests at one line on a phone', () => {
    expect(
      resolveChatComposerLayout(28, CHAT_COMPOSER_COMPACT_MIN_HEIGHT)
    ).toEqual({ height: 40, overflowY: 'hidden' })
    expect(
      resolveChatComposerLayout(90, CHAT_COMPOSER_COMPACT_MIN_HEIGHT).height
    ).toBe(90)
  })

  test('grows with multiline content', () => {
    expect(resolveChatComposerLayout(156)).toEqual({
      height: 156,
      overflowY: 'hidden',
    })
  })

  test('caps long drafts and enables an internal scrollbar', () => {
    expect(resolveChatComposerLayout(420)).toEqual({
      height: 240,
      overflowY: 'auto',
    })
  })
})

describe('chat composer interruption', () => {
  test('keeps an existing chat editable while a response streams', () => {
    expect(isChatComposerDisabled(true, false, false)).toBe(false)
    expect(chatComposerSubmitAction('change direction', true)).toBe(
      'interrupt-and-send'
    )
  })

  test('does not submit an empty interruption', () => {
    expect(chatComposerSubmitAction('   ', true)).toBe('none')
  })
})

describe('enterSendsMessage', () => {
  const enter = { key: 'Enter', shiftKey: false }

  test('sends on a computer', () => {
    expect(enterSendsMessage(enter, false)).toBe(true)
  })

  test('shift+enter and other keys never send', () => {
    expect(enterSendsMessage({ ...enter, shiftKey: true }, false)).toBe(false)
    expect(enterSendsMessage({ key: 'a', shiftKey: false }, false)).toBe(false)
  })

  test('confirming an IME candidate never sends', () => {
    expect(enterSendsMessage({ ...enter, isComposing: true }, false)).toBe(
      false
    )
  })

  test('is a new line on a phone', () => {
    expect(enterSendsMessage(enter, true)).toBe(false)
  })
})

describe('chat list pagination', () => {
  const chat = (id: string, minute: number) => ({
    public_id: id,
    last_activity_at: `2026-10-19T10:${String(minute).padStart(2, '0')}:00+00:00`,
  })
  const ids = (chats: { public_id: string }[]) =>
    chats.map((item) => item.public_id)

  test('the next page re-reads a partly seen page rather than skipping', () => {
    expect(nextChatListPage(0, 30)).toBe(1)
    expect(nextChatListPage(30, 30)).toBe(2)
    // One chat of the first page was deleted: page 1 again, not page 2.
    expect(nextChatListPage(29, 30)).toBe(1)
  })

  test('a refresh covers every loaded page, up to the endpoint maximum', () => {
    expect(chatListRefreshSize(0, 30)).toBe(30)
    expect(chatListRefreshSize(30, 30)).toBe(30)
    expect(chatListRefreshSize(31, 30)).toBe(60)
    expect(chatListRefreshSize(400, 30)).toBe(100)
  })

  test('older pages append without repeats and update what was shown', () => {
    const shown = [chat('a', 50), chat('b', 40)]
    const page = [{ ...chat('b', 40), title: 'renamed' }, chat('c', 30)]
    const merged = appendChatPage(shown, page)
    expect(ids(merged)).toEqual(['a', 'b', 'c'])
    expect(merged[1]).toEqual(page[0])
  })

  test('a full refresh keeps the older chats that were loaded', () => {
    const shown = [
      chat('gone', 55),
      chat('a', 50),
      chat('c', 30),
      chat('d', 20),
    ]
    const fresh = [chat('new', 59), chat('a', 50)]
    const result = refreshChatList(shown, fresh, 2, false)
    // `gone` is newer than the fresh page's oldest yet missing: deleted.
    expect(ids(result.chats)).toEqual(['new', 'a', 'c', 'd'])
    expect(result.hasMore).toBe(false)
  })

  test('a full refresh with nothing older loaded means there is more', () => {
    const result = refreshChatList([], [chat('a', 50), chat('b', 40)], 2, false)
    expect(result.hasMore).toBe(true)
  })

  test('a short refresh is the whole list', () => {
    const shown = [chat('a', 50), chat('old', 10)]
    const result = refreshChatList(shown, [chat('a', 50)], 30, true)
    expect(ids(result.chats)).toEqual(['a'])
    expect(result.hasMore).toBe(false)
  })

  test('a linked chat is added once, in activity order', () => {
    const shown = [chat('a', 50), chat('b', 40)]
    expect(ids(withLinkedChat(shown, chat('old', 5)))).toEqual([
      'a',
      'b',
      'old',
    ])
    expect(withLinkedChat(shown, chat('a', 50))).toBe(shown)
  })

  test('search matches already in the list are not offered twice', () => {
    const shown = [chat('a', 50), chat('b', 40)]
    const matches = [chat('b', 40), chat('old', 5), chat('older', 1)]
    expect(ids(chatsNotListed(shown, matches))).toEqual(['old', 'older'])
    expect(chatsNotListed([], matches)).toEqual(matches)
  })
})
