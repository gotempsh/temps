// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { threadTitleFromLiveEvent } from './thread-title-event'

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
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
