// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatElapsed,
  isLoginActive,
  isLoginTerminal,
  loginButtonLabel,
  loginCodeSchema,
  loginPhaseTitle,
  INITIAL_SUCCESS_ANNOUNCEMENT,
  MAX_LOGIN_CODE_LENGTH,
  nextSuccessAnnouncement,
} from './harness-login'

describe('harness login helpers', () => {
  test('terminal and active phases are complementary', () => {
    for (const phase of ['succeeded', 'failed', 'cancelled']) {
      expect(isLoginTerminal(phase)).toBe(true)
      expect(isLoginActive(phase)).toBe(false)
    }
    for (const phase of ['starting', 'waiting', 'verifying']) {
      expect(isLoginTerminal(phase)).toBe(false)
      expect(isLoginActive(phase)).toBe(true)
    }
    expect(isLoginTerminal(undefined)).toBe(false)
    expect(isLoginActive(undefined)).toBe(false)
  })

  test('formats elapsed time as m:ss', () => {
    expect(formatElapsed(0)).toBe('0:00')
    expect(formatElapsed(7)).toBe('0:07')
    expect(formatElapsed(61)).toBe('1:01')
    expect(formatElapsed(600)).toBe('10:00')
    expect(formatElapsed(-5)).toBe('0:00')
  })

  test('names the button after the provider, not the product line', () => {
    expect(loginButtonLabel('Claude Code')).toBe('Sign in with Claude')
    expect(loginButtonLabel('Codex (OpenAI)')).toBe('Sign in with Codex')
  })

  test('every phase has a heading', () => {
    expect(loginPhaseTitle('waiting', 'Claude Code')).toBe('Waiting for you')
    expect(loginPhaseTitle('failed', 'Claude Code')).toBe('Sign-in failed')
    expect(loginPhaseTitle('starting', 'Claude Code')).toContain('Claude Code')
  })

  test('the pasted code is validated like the server does', () => {
    const parse = (code: string) => loginCodeSchema.safeParse({ code })
    expect(parse('abc123#state').success).toBe(true)
    // Surrounding whitespace from a copy is trimmed, not rejected.
    const trimmed = parse('  abc123  ')
    expect(trimmed.success && trimmed.data.code).toBe('abc123')
    expect(parse('   ').success).toBe(false)
    expect(parse('two words').success).toBe(false)
    expect(parse('line\nbreak').success).toBe(false)
    expect(parse('caf\u00e9').success).toBe(false)
    expect(parse('a'.repeat(MAX_LOGIN_CODE_LENGTH)).success).toBe(true)
    expect(parse('a'.repeat(MAX_LOGIN_CODE_LENGTH + 1)).success).toBe(false)
  })

  test('a sign-in finished before the panel mounted is not announced again', () => {
    let state = INITIAL_SUCCESS_ANNOUNCEMENT
    // Stale success from an earlier sign-in is the baseline, not news.
    let step = nextSuccessAnnouncement(state, 'succeeded', 's1')
    expect(step.announce).toBe(false)
    state = step.state
    step = nextSuccessAnnouncement(state, 'succeeded', 's1')
    expect(step.announce).toBe(false)
    state = step.state

    // A new sign-in started here is announced exactly once when it succeeds.
    step = nextSuccessAnnouncement(state, 'starting', 's2')
    expect(step.announce).toBe(false)
    state = step.state
    step = nextSuccessAnnouncement(state, 'succeeded', 's2')
    expect(step.announce).toBe(true)
    state = step.state
    expect(nextSuccessAnnouncement(state, 'succeeded', 's2').announce).toBe(
      false
    )
  })

  test('a sign-in that was already running when the panel mounted is announced', () => {
    let step = nextSuccessAnnouncement(
      INITIAL_SUCCESS_ANNOUNCEMENT,
      'waiting',
      's1'
    )
    expect(step.announce).toBe(false)
    step = nextSuccessAnnouncement(step.state, 'succeeded', 's1')
    expect(step.announce).toBe(true)
  })
})
