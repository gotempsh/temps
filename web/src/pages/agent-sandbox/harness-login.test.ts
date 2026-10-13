// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatElapsed,
  isLoginActive,
  isLoginTerminal,
  loginButtonLabel,
  loginPhaseTitle,
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
})
