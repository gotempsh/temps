// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { formatFunnelDuration } from './funnel-duration'
import {
  FUNNEL_DEFAULT_RANGE_KEY,
  FUNNEL_QUICK_RANGES,
  funnelQuickRange,
  funnelQuickRangeBounds,
} from './funnel-quick-ranges'

describe('funnelQuickRangeBounds', () => {
  const now = new Date('2026-08-21T14:30:00.000Z')

  test('resolves the last 24 hours as a trailing window, not since midnight', () => {
    const bounds = funnelQuickRangeBounds('24h', now)

    expect(bounds?.to.toISOString()).toBe('2026-08-21T14:30:00.000Z')
    expect(bounds?.from.toISOString()).toBe('2026-08-20T14:30:00.000Z')
  })

  test('resolves the longer presets', () => {
    expect(funnelQuickRangeBounds('7d', now)?.from.toISOString()).toBe(
      '2026-08-14T14:30:00.000Z'
    )
    expect(funnelQuickRangeBounds('30d', now)?.from.toISOString()).toBe(
      '2026-07-22T14:30:00.000Z'
    )
    expect(funnelQuickRangeBounds('90d', now)?.from.toISOString()).toBe(
      '2026-05-23T14:30:00.000Z'
    )
  })

  test('returns nothing for an unknown preset', () => {
    expect(funnelQuickRangeBounds('all-time', now)).toBeUndefined()
    expect(funnelQuickRange('all-time')).toBeUndefined()
  })

  test('every preset is resolvable and the default is one of them', () => {
    for (const range of FUNNEL_QUICK_RANGES) {
      expect(funnelQuickRangeBounds(range.key, now)).toBeDefined()
      expect(range.description).not.toBe('')
    }
    expect(funnelQuickRange(FUNNEL_DEFAULT_RANGE_KEY)).toBeDefined()
  })

  test('preserves the previous 30-day default window', () => {
    // The funnel used to open on subDays(new Date(), 30); changing that
    // silently would move everyone's numbers.
    expect(funnelQuickRange(FUNNEL_DEFAULT_RANGE_KEY)?.hours).toBe(24 * 30)
  })
})

describe('formatFunnelDuration', () => {
  test('keeps precision the single-unit rounding used to destroy', () => {
    // Both of these rendered as "1h" before.
    expect(formatFunnelDuration(3540).primary).toBe('59m')
    expect(formatFunnelDuration(5340).primary).toBe('1h 29m')
  })

  test('always reports the exact seconds alongside', () => {
    expect(formatFunnelDuration(5340).exact).toBe('5,340 seconds')
    expect(formatFunnelDuration(42).exact).toBe('42 seconds')
    expect(formatFunnelDuration(1).exact).toBe('1 second')
  })

  test('drops empty trailing units', () => {
    expect(formatFunnelDuration(7200).primary).toBe('2h')
    expect(formatFunnelDuration(300).primary).toBe('5m')
    expect(formatFunnelDuration(312).primary).toBe('5m 12s')
    expect(formatFunnelDuration(42).primary).toBe('42s')
  })

  test('rounds fractional seconds', () => {
    expect(formatFunnelDuration(41.6).primary).toBe('42s')
    expect(formatFunnelDuration(41.6).exact).toBe('42 seconds')
  })

  test('treats missing or nonsensical values as zero rather than rendering NaN', () => {
    for (const value of [0, -12, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(formatFunnelDuration(value)).toEqual({
        primary: '0s',
        exact: '0 seconds',
      })
    }
  })
})
