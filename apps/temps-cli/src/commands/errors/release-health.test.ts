// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { formatRate, MAX_HEALTH_DAYS, parseHealthDays, releaseLabel } from './release-health.js'

describe('parseHealthDays', () => {
  test('accepts whole days up to the maximum', () => {
    expect(parseHealthDays('1')).toEqual({ ok: true, days: 1 })
    expect(parseHealthDays(String(MAX_HEALTH_DAYS))).toEqual({ ok: true, days: MAX_HEALTH_DAYS })
  })

  test('rejects zero, fractions, junk and too many days, naming the flag', () => {
    for (const bad of ['0', '2.5', 'week', String(MAX_HEALTH_DAYS + 1)]) {
      const result = parseHealthDays(bad)
      expect(result.ok).toBe(false)
      if (!result.ok) expect(result.error).toContain('--days')
    }
  })
})

describe('formatRate', () => {
  test('never rounds a rate below 100% up to 100%', () => {
    expect(formatRate(0.99999)).toBe('99.99%')
    expect(formatRate(1)).toBe('100%')
    expect(formatRate(0.95)).toBe('95.0%')
    expect(formatRate(null)).toBe('-')
  })
})

describe('releaseLabel', () => {
  test('shortens commit SHAs only', () => {
    expect(releaseLabel('0123456789abcdef0123456789abcdef01234567')).toBe('0123456789ab')
    expect(releaseLabel('my-app@1.2.3')).toBe('my-app@1.2.3')
  })
})
