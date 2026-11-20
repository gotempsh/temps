// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  syncedPageParams,
  withoutChat,
  withoutChatScopedParams,
  withParam,
} from './assistant-url'

const q = (s: string) => new URLSearchParams(s)

describe('withParam', () => {
  test('sets and removes without touching the original', () => {
    const base = q('chat=a&debug=true')
    expect(withParam(base, 'tab', 'preview').toString()).toBe(
      'chat=a&debug=true&tab=preview'
    )
    expect(withParam(base, 'chat', null).toString()).toBe('debug=true')
    expect(base.toString()).toBe('chat=a&debug=true')
  })

  test('treats an empty value as removal', () => {
    expect(withParam(q('tab=made'), 'tab', '').has('tab')).toBe(false)
  })
})

describe('withoutChat', () => {
  test('drops the chat and its tab state, keeps the rest', () => {
    const next = withoutChat(q('chat=a&view=work&tab=terminal&panel=open&x=1'))
    expect(next.toString()).toBe('view=work&x=1')
  })

  test('scoped variant keeps the chat', () => {
    expect(
      withoutChatScopedParams(q('chat=a&tab=made&panel=open')).toString()
    ).toBe('chat=a')
  })
})

describe('syncedPageParams', () => {
  test('is null when the address already says it', () => {
    expect(syncedPageParams(q('chat=a&tab=made'), { chat: 'a' })).toBeNull()
  })

  test('keeps the tab when a reload resolves the chat the address names', () => {
    expect(
      syncedPageParams(q('chat=a&tab=terminal&panel=open'), { chat: 'a' })
    ).toBeNull()
  })

  test('writes the chat into a bare address', () => {
    expect(syncedPageParams(q(''), { chat: 'a' })?.toString()).toBe('chat=a')
  })

  test('switching chats drops the previous chat tab state', () => {
    expect(
      syncedPageParams(q('chat=a&tab=terminal&panel=open&x=1'), {
        chat: 'b',
      })?.toString()
    ).toBe('chat=b&x=1')
  })

  test('drops the retired layout from links copied before it went away', () => {
    expect(
      syncedPageParams(q('chat=a&view=work&tab=made'), {
        chat: 'a',
      })?.toString()
    ).toBe('chat=a&tab=made')
  })

  test('does not invent a chat while none is open', () => {
    expect(syncedPageParams(q(''), { chat: null })).toBeNull()
  })
})
