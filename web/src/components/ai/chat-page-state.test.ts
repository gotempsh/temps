// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  chatComposerSubmitAction,
  conversationListNeedsRefresh,
  createProjectChat,
  isChatComposerDisabled,
  resolveChatComposerLayout,
  resolvePageChat,
} from './chat-page-state'

const conversations = [
  { public_id: 'most-recent' },
  { public_id: 'older-chat' },
]

describe('resolvePageChat', () => {
  test('redirects a bare chat route to the first conversation', () => {
    expect(resolvePageChat(null, conversations)?.public_id).toBe('most-recent')
  })

  test('keeps the conversation selected by the URL across reloads', () => {
    expect(resolvePageChat('older-chat', conversations)?.public_id).toBe(
      'older-chat'
    )
  })

  test('redirects a stale conversation URL to the first conversation', () => {
    expect(resolvePageChat('deleted-chat', conversations)?.public_id).toBe(
      'most-recent'
    )
  })

  test('leaves the route empty when there are no conversations', () => {
    expect(resolvePageChat(null, [])).toBeNull()
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
      title: 'Project chat',
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
