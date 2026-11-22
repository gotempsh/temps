// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CHAT_LIST_STORAGE_KEY,
  isChatListShortcut,
  isFocusComposerShortcut,
  isSwitcherShortcut,
  readChatListHidden,
  writeChatListHidden,
} from './assistant-view'

function memory(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial))
  return {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
  }
}

describe('chat list visibility', () => {
  test('is shown until the user hides it, and the choice survives a reload', () => {
    const storage = memory()
    expect(readChatListHidden(storage)).toBe(false)
    writeChatListHidden(true, storage)
    expect(storage.getItem(CHAT_LIST_STORAGE_KEY)).toBe('hidden')
    expect(readChatListHidden(storage)).toBe(true)
    writeChatListHidden(false, storage)
    expect(readChatListHidden(storage)).toBe(false)
  })

  test('the retired work layout carries over as a hidden list', () => {
    expect(readChatListHidden(memory({ 'temps.assistant.view': 'work' }))).toBe(
      true
    )
    expect(
      readChatListHidden(memory({ 'temps.assistant.view': 'chats' }))
    ).toBe(false)
    // A choice made since wins over the old layout.
    expect(
      readChatListHidden(
        memory({
          'temps.assistant.view': 'work',
          [CHAT_LIST_STORAGE_KEY]: 'shown',
        })
      )
    ).toBe(false)
  })

  test('blocked storage shows the list without throwing', () => {
    const blocked = {
      getItem: () => {
        throw new Error('denied')
      },
      setItem: () => {
        throw new Error('denied')
      },
    }
    expect(readChatListHidden(blocked)).toBe(false)
    expect(() => writeChatListHidden(true, blocked)).not.toThrow()
    expect(readChatListHidden(null)).toBe(false)
  })
})

describe('isChatListShortcut', () => {
  const press = (over: Partial<KeyboardEvent>) => ({
    key: 'b',
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...over,
  })

  test('is ⌘B or Ctrl+B, in either case', () => {
    expect(isChatListShortcut(press({ metaKey: true }))).toBe(true)
    expect(isChatListShortcut(press({ ctrlKey: true, key: 'B' }))).toBe(true)
  })

  test('is not a bare B or a modified chord', () => {
    expect(isChatListShortcut(press({}))).toBe(false)
    expect(isChatListShortcut(press({ metaKey: true, shiftKey: true }))).toBe(
      false
    )
    expect(isChatListShortcut(press({ metaKey: true, altKey: true }))).toBe(
      false
    )
    expect(isChatListShortcut(press({ metaKey: true, key: 'k' }))).toBe(false)
  })
})

describe('isSwitcherShortcut', () => {
  const press = (over: Partial<KeyboardEvent>) => ({
    key: 'k',
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...over,
  })
  test('is Cmd+K or Ctrl+K, nothing more', () => {
    expect(isSwitcherShortcut(press({ metaKey: true }))).toBe(true)
    expect(isSwitcherShortcut(press({ ctrlKey: true, key: 'K' }))).toBe(true)
    expect(isSwitcherShortcut(press({}))).toBe(false)
    expect(isSwitcherShortcut(press({ metaKey: true, shiftKey: true }))).toBe(
      false
    )
    expect(isSwitcherShortcut(press({ metaKey: true, key: 'j' }))).toBe(false)
  })
})

describe('isFocusComposerShortcut', () => {
  const press = (over: Record<string, unknown> = {}) => ({
    key: '/',
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    isComposing: false,
    defaultPrevented: false,
    target: { tagName: 'BODY' } as unknown as EventTarget,
    ...over,
  })
  const at = (target: Record<string, unknown>) =>
    press({ target: target as unknown as EventTarget })

  test('is a bare slash on the page', () => {
    expect(isFocusComposerShortcut(press())).toBe(true)
    expect(isFocusComposerShortcut(press({ target: null }))).toBe(true)
  })

  test('is not a slash typed into a field', () => {
    expect(isFocusComposerShortcut(at({ tagName: 'TEXTAREA' }))).toBe(false)
    expect(isFocusComposerShortcut(at({ tagName: 'input' }))).toBe(false)
    expect(isFocusComposerShortcut(at({ tagName: 'SELECT' }))).toBe(false)
    expect(
      isFocusComposerShortcut(at({ tagName: 'DIV', isContentEditable: true }))
    ).toBe(false)
  })

  test('leaves focus alone inside a dialog or menu', () => {
    expect(
      isFocusComposerShortcut(at({ tagName: 'BUTTON', closest: () => ({}) }))
    ).toBe(false)
    expect(
      isFocusComposerShortcut(at({ tagName: 'BUTTON', closest: () => null }))
    ).toBe(true)
  })

  test('ignores chords, compositions and handled keys', () => {
    expect(isFocusComposerShortcut(press({ metaKey: true }))).toBe(false)
    expect(isFocusComposerShortcut(press({ ctrlKey: true }))).toBe(false)
    expect(isFocusComposerShortcut(press({ altKey: true }))).toBe(false)
    expect(isFocusComposerShortcut(press({ isComposing: true }))).toBe(false)
    expect(isFocusComposerShortcut(press({ defaultPrevented: true }))).toBe(
      false
    )
    expect(isFocusComposerShortcut(press({ key: 'k' }))).toBe(false)
  })
})
