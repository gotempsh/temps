import { describe, expect, test } from 'bun:test'

import {
  ENABLE_AI_WRITES_BODY,
  focusedStartPrompt,
  suggestAlertsState,
} from './suggest-alerts-state'

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
    ).toBe('enable-writes')
  })

  test('opens chat when ready or lets chat surface a readiness error', () => {
    expect(
      suggestAlertsState({
        isPending: false,
        isError: false,
        aiConfigured: true,
        writeEnabled: true,
      })
    ).toBe('ready')
    expect(suggestAlertsState({ isPending: false, isError: true })).toBe(
      'ready'
    )
  })

  test('enables chat and confirm-gated writes together', () => {
    expect(ENABLE_AI_WRITES_BODY).toEqual({
      ai_write_actions_enabled: true,
      ai_debug_chat_enabled: true,
    })
  })

  test('focused prompt requires real values and duplicate checking', () => {
    const prompt = focusedStartPrompt('http.server.duration')
    expect(prompt).toContain('http.server.duration')
    expect(prompt).toContain('Query its real values')
    expect(prompt).toContain("don't duplicate")
  })
})
