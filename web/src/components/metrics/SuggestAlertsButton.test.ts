// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { focusedStartPrompt, suggestAlertsState } from './suggest-alerts-state'

describe('suggestAlertsState', () => {
  test('waits for readiness before offering an action', () => {
    expect(suggestAlertsState({ isPending: true, isError: false })).toBe(
      'loading'
    )
  })

  test('routes each missing prerequisite to its setup path', () => {
    expect(suggestAlertsState({ isPending: false, isError: false })).toBe(
      'configure-provider'
    )
    expect(
      suggestAlertsState({
        isPending: false,
        isError: false,
        aiConfigured: true,
      })
    ).toBe('ready')
  })

  test('opens chat when ready or lets chat surface a readiness error', () => {
    expect(
      suggestAlertsState({
        isPending: false,
        isError: false,
        aiConfigured: true,
      })
    ).toBe('ready')
    expect(suggestAlertsState({ isPending: false, isError: true })).toBe(
      'ready'
    )
  })

  test('focused prompt requires real values and duplicate checking', () => {
    const prompt = focusedStartPrompt('http.server.duration')
    expect(prompt).toContain('http.server.duration')
    expect(prompt).toContain('Query its real values')
    expect(prompt).toContain("don't duplicate")
  })
})
