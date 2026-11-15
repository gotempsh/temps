// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  groupChatsByWorkspace,
  type GroupableChat,
} from './conversation-groups'

const shop = { public_id: 'app_shop', name: 'shop' }
const blog = { public_id: 'app_blog', name: 'blog' }
const chat = (id: string, workspace?: GroupableChat['workspace']) => ({
  public_id: id,
  workspace,
})

describe('groupChatsByWorkspace', () => {
  test('keeps chats outside a workspace as single rows in order', () => {
    const entries = groupChatsByWorkspace([chat('a'), chat('b', null)])
    expect(
      entries.map((e) => (e.kind === 'chat' ? e.chat.public_id : ''))
    ).toEqual(['a', 'b'])
  })

  test('gathers a workspace where its newest chat is', () => {
    const entries = groupChatsByWorkspace([
      chat('a'),
      chat('b', shop),
      chat('c'),
      chat('d', shop),
      chat('e', blog),
    ])
    expect(
      entries.map((e) =>
        e.kind === 'chat'
          ? e.chat.public_id
          : `${e.workspace.name}:${e.chats.map((c) => c.public_id).join(',')}`
      )
    ).toEqual(['a', 'shop:b,d', 'c', 'blog:e'])
  })

  test('a workspace with one chat still gets its own entry', () => {
    const [entry] = groupChatsByWorkspace([chat('a', blog)])
    expect(entry.kind).toBe('workspace')
  })

  test('an empty list has no entries', () => {
    expect(groupChatsByWorkspace([])).toEqual([])
  })
})
