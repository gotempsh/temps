// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { contextLabel, contextLevel, contextShare } from './chat-spend'

describe('context readout', () => {
  test('a model with a window shows tokens against it', () => {
    expect(contextLabel({ tokens: 38_500, window: 1_000_000 })).toBe(
      '38.5k of 1.0M tokens (4%)'
    )
    expect(contextShare({ tokens: 500_000, window: 1_000_000 })).toBe(0.5)
  })

  test('a model without a listed window still shows the tokens', () => {
    expect(contextLabel({ tokens: 1_200 })).toBe('1.2k tokens')
    expect(contextShare({ tokens: 1_200, window: null })).toBeNull()
    expect(contextLevel({ tokens: 900_000 })).toBe('ok')
  })

  test('a tiny share reads as under one percent instead of zero', () => {
    expect(contextLabel({ tokens: 900, window: 1_000_000 })).toBe(
      '900 of 1.0M tokens (<1%)'
    )
  })

  test('the level warns before the window is full', () => {
    expect(contextLevel({ tokens: 100, window: 200 })).toBe('ok')
    expect(contextLevel({ tokens: 130, window: 200 })).toBe('high')
    expect(contextLevel({ tokens: 180, window: 200 })).toBe('critical')
    expect(contextShare({ tokens: 300, window: 200 })).toBe(1)
  })
})
