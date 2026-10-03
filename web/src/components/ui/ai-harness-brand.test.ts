// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { aiHarnessName, canonicalHarnessId } from './ai-harness-brand'

describe('canonicalHarnessId', () => {
  test('recognizes the exact pi provider id, ignoring case and padding', () => {
    expect(canonicalHarnessId('pi')).toBe('pi')
    expect(canonicalHarnessId(' PI ')).toBe('pi')
  })

  test('does not treat words containing "pi" as the pi harness', () => {
    for (const value of ['api', 'pipeline', 'spinner', 'pi_cli', 'pi-agent']) {
      expect(canonicalHarnessId(value)).toBe(value)
    }
    expect(canonicalHarnessId('anthropic_api')).toBe('claude_cli')
    expect(canonicalHarnessId('openai')).toBe('codex_cli')
  })
})

describe('aiHarnessName', () => {
  test('keeps pi lowercase, as the product names itself', () => {
    expect(aiHarnessName('pi')).toBe('pi')
  })

  test('names the other first-class harnesses and passes unknown ids through', () => {
    expect(aiHarnessName('claude_cli')).toBe('Claude Code')
    expect(aiHarnessName('codex_cli')).toBe('Codex')
    expect(aiHarnessName('opencode')).toBe('OpenCode')
    expect(aiHarnessName('api')).toBe('api')
  })
})
