// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  anyChatWorking,
  chatListPollInterval,
  IDLE_CHAT_POLL_MS,
  WORKING_CHAT_POLL_MS,
  chatRunPresentation,
  chatRunState,
  railMarksState,
  withTurnStatus,
} from './chat-run-state'

describe('chatRunState', () => {
  it('maps each turn status to what the list shows', () => {
    expect(chatRunState('running')).toBe('working')
    expect(chatRunState('completed')).toBe('finished')
    expect(chatRunState('failed')).toBe('error')
    expect(chatRunState('cancelled')).toBe('stopped')
    expect(chatRunState('interrupted')).toBe('interrupted')
  })

  it('shows nothing for a chat that never ran a turn', () => {
    expect(chatRunState('idle')).toBeNull()
    expect(chatRunState('')).toBeNull()
  })
})

describe('chatRunPresentation', () => {
  it('names the failure on hover when the server reported one', () => {
    expect(chatRunPresentation('error', 'Provider rate limited').title).toBe(
      'The last turn failed: Provider rate limited'
    )
    expect(chatRunPresentation('error').title).toBe('The last turn failed')
  })

  it('labels every state', () => {
    for (const state of [
      'working',
      'finished',
      'error',
      'stopped',
      'interrupted',
    ] as const) {
      expect(chatRunPresentation(state).label.length).toBeGreaterThan(0)
    }
  })
})

describe('anyChatWorking', () => {
  it('is true only while a chat is running', () => {
    expect(anyChatWorking([{ turn_status: 'completed' }])).toBe(false)
    expect(
      anyChatWorking([{ turn_status: 'completed' }, { turn_status: 'running' }])
    ).toBe(true)
  })
})

describe('chatListPollInterval', () => {
  it('checks often while a chat works and rarely otherwise', () => {
    expect(chatListPollInterval([{ turn_status: 'running' }])).toBe(
      WORKING_CHAT_POLL_MS
    )
    expect(chatListPollInterval([{ turn_status: 'completed' }])).toBe(
      IDLE_CHAT_POLL_MS
    )
    expect(chatListPollInterval([])).toBe(IDLE_CHAT_POLL_MS)
  })
})

describe('withTurnStatus', () => {
  const chats = [
    { public_id: 'a', turn_status: 'completed' },
    { public_id: 'b', turn_status: 'idle' },
  ]

  it('replaces one chat status', () => {
    const next = withTurnStatus(chats, 'b', 'running')
    expect(next.map((chat) => chat.turn_status)).toEqual([
      'completed',
      'running',
    ])
    expect(chats[1].turn_status).toBe('idle')
  })

  it('returns the same list when nothing changes', () => {
    expect(withTurnStatus(chats, 'a', 'completed')).toBe(chats)
    expect(withTurnStatus(chats, 'missing', 'running')).toBe(chats)
  })
})

describe('railMarksState', () => {
  it('marks the chats that need a look and leaves done ones plain', () => {
    expect(railMarksState(chatRunState('running'))).toBe(true)
    expect(railMarksState(chatRunState('failed'))).toBe(true)
    expect(railMarksState(chatRunState('interrupted'))).toBe(true)
    expect(railMarksState(chatRunState('completed'))).toBe(false)
    expect(railMarksState(chatRunState('cancelled'))).toBe(false)
    expect(railMarksState(chatRunState('idle'))).toBe(false)
  })
})
