// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { mergeConversationPages } from './AiFirstWorkspace'

describe('thread pagination', () => {
  test('keeps first-page order and deduplicates overlapping later pages', () => {
    const merged = mergeConversationPages(
      [{ public_id: 'newest' }, { public_id: 'boundary' }],
      [{ public_id: 'boundary' }, { public_id: 'older' }]
    )

    expect(merged.map((conversation) => conversation.public_id)).toEqual([
      'newest',
      'boundary',
      'older',
    ])
  })
})
