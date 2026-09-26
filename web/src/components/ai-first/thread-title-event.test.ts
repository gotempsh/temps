// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  threadTitleFromLiveEvent,
  workspacePageTitle,
} from './thread-title-event'

describe('workspacePageTitle', () => {
  test('shows the thread followed by its workspace', () => {
    expect(workspacePageTitle('My app', 'Fix login')).toBe('Fix login · My app')
  })
  test('avoids repeating identical thread and workspace names', () => {
    expect(workspacePageTitle(' My app ', 'My app')).toBe('My app')
  })
  test('handles loading, untitled threads, and the default workspace', () => {
    expect(workspacePageTitle('My app', null)).toBe('My app')
    expect(workspacePageTitle(undefined, 'Fix login')).toBe('Fix login')
    expect(workspacePageTitle(null, '  ')).toBe('')
    expect(workspacePageTitle('Default workspace', 'Hello')).toBe(
      'Hello · Default workspace'
    )
  })
})

describe('threadTitleFromLiveEvent', () => {
  test('returns a stored harness title', () => {
    expect(
      threadTitleFromLiveEvent(
        'conversation_title',
        JSON.stringify({ title: 'Create MongoDB Instance' })
      )
    ).toBe('Create MongoDB Instance')
  })

  test('ignores unrelated and malformed events', () => {
    expect(threadTitleFromLiveEvent('turn_complete', '{}')).toBeNull()
    expect(threadTitleFromLiveEvent('conversation_title', '{')).toBeNull()
    expect(
      threadTitleFromLiveEvent(
        'conversation_title',
        JSON.stringify({ title: '   ' })
      )
    ).toBeNull()
  })
})
