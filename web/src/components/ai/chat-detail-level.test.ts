// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { DEFAULT_CHAT_DETAIL, chatDetailFor } from './chat-detail-level'

describe('chatDetailFor', () => {
  test('simple hides the pickers and keeps tool calls collapsed', () => {
    expect(chatDetailFor('simple')).toEqual({
      showRuntimeControls: false,
      expandToolCalls: false,
    })
  })

  test('advanced shows the pickers and opens tool calls', () => {
    expect(chatDetailFor('advanced')).toEqual({
      showRuntimeControls: true,
      expandToolCalls: true,
    })
  })

  test('an unset level keeps every existing chat surface unchanged', () => {
    expect(chatDetailFor(undefined)).toBe(DEFAULT_CHAT_DETAIL)
    expect(DEFAULT_CHAT_DETAIL).toEqual({
      showRuntimeControls: true,
      expandToolCalls: false,
    })
  })
})
