// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ChatAttachment } from './chat-message-parts'
import {
  isQueueShortcut,
  mergeQueuedMessages,
  queueShortcutLabel,
  readChatQueue,
  updateChatQueue,
} from './chat-message-queue'

const file = (id: string): ChatAttachment => ({
  id,
  name: `${id}.png`,
  mime_type: 'image/png',
  size_bytes: 10,
  sandbox_path: `/tmp/${id}.png`,
  is_image: true,
})

describe('chat message queue', () => {
  test('keeps each conversation’s queue apart and forgets an emptied one', () => {
    updateChatQueue('chat-a', (queue) => [
      ...queue,
      { id: '1', text: 'first', attachments: [] },
    ])
    updateChatQueue('chat-b', (queue) => [
      ...queue,
      { id: '2', text: 'other chat', attachments: [] },
    ])

    expect(readChatQueue('chat-a').map((m) => m.text)).toEqual(['first'])
    expect(readChatQueue('chat-b').map((m) => m.text)).toEqual(['other chat'])

    updateChatQueue('chat-a', () => [])
    expect(readChatQueue('chat-a')).toEqual([])
    updateChatQueue('chat-b', () => [])
  })

  test('sends everything queued as one message, in order', () => {
    expect(
      mergeQueuedMessages([
        { id: '1', text: ' check the logs ', attachments: [file('a')] },
        { id: '2', text: '   ', attachments: [] },
        { id: '3', text: 'then redeploy', attachments: [file('b')] },
      ])
    ).toEqual({
      text: 'check the logs\n\nthen redeploy',
      attachments: [file('a'), file('b')],
    })
  })
})

describe('isQueueShortcut', () => {
  const enter = { key: 'Enter', metaKey: false, ctrlKey: false }

  test('⌘Enter and Ctrl+Enter queue', () => {
    expect(isQueueShortcut({ ...enter, metaKey: true })).toBe(true)
    expect(isQueueShortcut({ ...enter, ctrlKey: true })).toBe(true)
  })

  test('plain Enter, other keys and IME confirmation do not', () => {
    expect(isQueueShortcut(enter)).toBe(false)
    expect(isQueueShortcut({ ...enter, key: 'k', metaKey: true })).toBe(false)
    expect(
      isQueueShortcut({ ...enter, metaKey: true, isComposing: true })
    ).toBe(false)
  })
})

test('the shortcut label follows the platform', () => {
  expect(queueShortcutLabel('Mozilla/5.0 (Macintosh; Intel Mac OS X)')).toBe(
    '⌘↵'
  )
  expect(queueShortcutLabel('Mozilla/5.0 (Windows NT 10.0)')).toBe('Ctrl+↵')
})
