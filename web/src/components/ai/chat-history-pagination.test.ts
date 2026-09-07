// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  prependHistoryPage,
  reconcileLatestHistoryPage,
  restoredHistoryScrollTop,
  shouldLoadEarlierMessages,
} from './chat-history-pagination'

type TestMessage = {
  server_cursor?: string
  content: string
}

describe('conversation history pagination', () => {
  test('prepends older messages and removes cursor duplicates', () => {
    const current: TestMessage[] = [
      { server_cursor: 'm1_3', content: 'three' },
      { server_cursor: 'm1_4', content: 'four' },
    ]
    const older: TestMessage[] = [
      { server_cursor: 'm1_1', content: 'one' },
      { server_cursor: 'm1_2', content: 'two' },
      { server_cursor: 'm1_3', content: 'duplicate' },
    ]

    expect(
      prependHistoryPage(current, older).map((message) => message.content)
    ).toEqual(['one', 'two', 'three', 'four'])
  })

  test('retains loaded history when the latest server page refreshes', () => {
    const current: TestMessage[] = [
      { server_cursor: 'm1_1', content: 'older' },
      { server_cursor: 'm1_2', content: 'old snapshot' },
      { content: 'optimistic placeholder' },
    ]
    const latest: TestMessage[] = [
      { server_cursor: 'm1_2', content: 'updated snapshot' },
      { server_cursor: 'm1_3', content: 'new reply' },
    ]

    expect(
      reconcileLatestHistoryPage(current, latest).map(
        (message) => message.content
      )
    ).toEqual(['older', 'updated snapshot', 'new reply'])
  })

  test('loads near the top and restores the same visible transcript row', () => {
    expect(shouldLoadEarlierMessages(96, true)).toBe(true)
    expect(shouldLoadEarlierMessages(97, true)).toBe(false)
    expect(shouldLoadEarlierMessages(0, false)).toBe(false)

    expect(
      restoredHistoryScrollTop({ scrollHeight: 2_000, scrollTop: 40 }, 3_250)
    ).toBe(1_290)
  })
})
