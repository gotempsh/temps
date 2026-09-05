// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ConversationResponse } from '@/api/client'
import {
  defaultWorkspaceSelection,
  initialApplicationThreadId,
  threadSelectionAfterRemoval,
} from './thread-selection'

function thread(
  publicId: string,
  createdAt: string,
  lastActivityAt: string
): ConversationResponse {
  return {
    public_id: publicId,
    project_id: null,
    context_type: 'application',
    context_id: `app:${publicId}`,
    title: 'Application thread',
    status: 'active',
    created_at: createdAt,
    last_activity_at: lastActivityAt,
    ai_provider: 'claude_cli',
    ai_model: 'sonnet',
    ai_thinking_level: 'high',
    ai_permission_mode: 'auto',
    turn_status: 'idle',
    application_id: 1,
  }
}

describe('application thread selection', () => {
  const emptyNewest = thread(
    'empty',
    '2026-09-01T11:00:00Z',
    '2026-09-01T11:00:00Z'
  )
  const existingHistory = thread(
    'history',
    '2026-08-31T14:00:00Z',
    '2026-09-01T10:30:00Z'
  )

  test('restores an explicit URL selection', () => {
    expect(
      initialApplicationThreadId([emptyNewest, existingHistory], 'empty')
    ).toBe('empty')
  })

  test('does not let a newer untouched thread hide existing history', () => {
    expect(
      initialApplicationThreadId([emptyNewest, existingHistory], null)
    ).toBe('history')
  })

  test('falls back to the first thread when all are untouched', () => {
    expect(initialApplicationThreadId([emptyNewest], null)).toBe('empty')
    expect(initialApplicationThreadId([], null)).toBeNull()
  })
})

describe('default workspace selection', () => {
  test('clears the application scope before opening an empty workspace', () => {
    expect(defaultWorkspaceSelection([])).toEqual({
      applicationId: null,
      conversationId: null,
      openStartScreen: true,
    })
  })

  test('selects an existing global thread without opening the start screen', () => {
    expect(defaultWorkspaceSelection(['global-1', 'global-2'])).toEqual({
      applicationId: null,
      conversationId: 'global-1',
      openStartScreen: false,
    })
  })
})

describe('archived thread selection', () => {
  test('selects the next thread when the open thread is archived', () => {
    expect(
      threadSelectionAfterRemoval(['current', 'next'], 'current', 'current')
    ).toBe('next')
  })

  test('clears the selection when the final thread is archived', () => {
    expect(
      threadSelectionAfterRemoval(['current'], 'current', 'current')
    ).toBeNull()
  })

  test('does not disturb another open thread', () => {
    expect(
      threadSelectionAfterRemoval(['open', 'archived'], 'open', 'archived')
    ).toBe('open')
  })
})
