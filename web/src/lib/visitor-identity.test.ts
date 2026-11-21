// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  IDENTITY_MODE_OPTIONS,
  cookielessFactLine,
  cookielessSnippets,
} from './visitor-identity'

describe('IDENTITY_MODE_OPTIONS', () => {
  test('offers both modes, persistent first', () => {
    expect(IDENTITY_MODE_OPTIONS.map((option) => option.value)).toEqual([
      'persistent',
      'cookieless',
    ])
  })

  test('the cookieless option states what it costs', () => {
    const cookieless = IDENTITY_MODE_OPTIONS[1]
    expect(cookieless.consequences.join(' ')).toContain('new visitor the next day')
    expect(cookieless.consequences.join(' ')).toContain('session replay is off')
  })
})

describe('cookielessSnippets', () => {
  test('tell both tracker flavours to store nothing', () => {
    const snippets = cookielessSnippets()
    expect(snippets.react).toContain('<TempsAnalyticsProvider cookieless>')
    expect(snippets.script).toContain('data-cookieless="true"')
  })
})

describe('cookielessFactLine', () => {
  test('each affected view gets its own sentence', () => {
    const lines = (['visitors', 'journey', 'replays'] as const).map(
      cookielessFactLine
    )
    expect(new Set(lines).size).toBe(3)
    for (const line of lines) expect(line.startsWith('Cookieless tracking:')).toBe(true)
  })
})
