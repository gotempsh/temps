// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  compactResultMessage,
  formatSpend,
  formatTokens,
  spendLabel,
  summarizedLabel,
} from './chat-spend'

describe('formatSpend', () => {
  test('converts microcents to dollars', () => {
    // 1 cent = 10,000 microcents.
    expect(formatSpend(10_000)).toBe('$0.0100')
    expect(formatSpend(1_234_500)).toBe('$1.23')
  })

  test('keeps fractions of a cent visible instead of showing $0.00', () => {
    expect(formatSpend(123)).toBe('$0.0001')
    expect(formatSpend(50)).toBe('<$0.0001')
  })

  test('shows an exact zero as $0.00 and never goes negative', () => {
    expect(formatSpend(0)).toBe('$0.00')
    expect(formatSpend(-5)).toBe('$0.00')
  })
})

describe('spendLabel', () => {
  test('a chat with no requests has not spent anything yet', () => {
    expect(
      spendLabel({ requests: 0, cost_microcents: 0, unpriced_requests: 0 })
    ).toBe('No spend yet')
  })

  test('a fully priced chat shows its total', () => {
    expect(
      spendLabel({
        requests: 4,
        cost_microcents: 20_000,
        unpriced_requests: 0,
      })
    ).toBe('$0.0200')
  })

  test('a partly unpriced chat is a floor, not a total', () => {
    expect(
      spendLabel({
        requests: 4,
        cost_microcents: 20_000,
        unpriced_requests: 1,
      })
    ).toBe('At least $0.0200')
  })

  test('a chat whose requests were all unpriced says the cost is unknown', () => {
    expect(
      spendLabel({ requests: 3, cost_microcents: 0, unpriced_requests: 3 })
    ).toBe('Cost unknown')
  })
})

describe('formatTokens', () => {
  test('abbreviates thousands and millions', () => {
    expect(formatTokens(999)).toBe('999')
    expect(formatTokens(1234)).toBe('1.2k')
    expect(formatTokens(2_500_000)).toBe('2.5M')
  })
})

describe('compactResultMessage', () => {
  test('says how many messages were folded', () => {
    expect(
      compactResultMessage({ compacted: true, folded_messages: 12 })
    ).toContain('Summarized 12 earlier messages')
    expect(
      compactResultMessage({ compacted: true, folded_messages: 1 })
    ).toContain('Summarized 1 earlier message.')
  })

  test('explains why nothing happened', () => {
    expect(
      compactResultMessage({
        compacted: false,
        folded_messages: 0,
        reason: 'Too few messages.',
      })
    ).toBe('Too few messages.')
    expect(
      compactResultMessage({ compacted: false, folded_messages: 0 })
    ).toBe('Nothing to compact yet.')
  })
})

describe('summarizedLabel', () => {
  test('is silent for a chat that was never compacted', () => {
    expect(summarizedLabel(0)).toBeNull()
    expect(summarizedLabel(-3)).toBeNull()
  })

  test('counts the summarized messages', () => {
    expect(summarizedLabel(1)).toContain('1 earlier message is summarized')
    expect(summarizedLabel(20)).toContain('20 earlier messages are summarized')
  })
})
