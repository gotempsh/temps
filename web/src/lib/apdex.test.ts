// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  apdexBreakdown,
  apdexRating,
  apdexThresholdFromParam,
  DEFAULT_APDEX_THRESHOLD,
} from './apdex'

describe('apdexRating', () => {
  test('uses the conventional bands, inclusive at each lower edge', () => {
    expect(apdexRating(1)).toBe('excellent')
    expect(apdexRating(0.94)).toBe('excellent')
    expect(apdexRating(0.9)).toBe('good')
    expect(apdexRating(0.7)).toBe('fair')
    expect(apdexRating(0.5)).toBe('poor')
    expect(apdexRating(0.49)).toBe('unacceptable')
    expect(apdexRating(0)).toBe('unacceptable')
  })
})

describe('apdexThresholdFromParam', () => {
  test('keeps an offered threshold and falls back otherwise', () => {
    expect(apdexThresholdFromParam('250')).toBe('250')
    expect(apdexThresholdFromParam(null)).toBe(DEFAULT_APDEX_THRESHOLD)
    expect(apdexThresholdFromParam('7')).toBe(DEFAULT_APDEX_THRESHOLD)
  })
})

describe('apdexBreakdown', () => {
  test('lists the three counts, or nothing when the row has no score', () => {
    expect(
      apdexBreakdown({
        satisfied_count: 12,
        tolerating_count: 3,
        frustrated_count: 1,
      })
    ).toBe('12 satisfied, 3 tolerating, 1 frustrated')
    expect(apdexBreakdown({ satisfied_count: null })).toBeNull()
  })
})
