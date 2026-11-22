// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { formatChange, formatSharePercent, MAX_TREND_DAYS, parseTrendDays, summarizeByDay } from './operations.js'

const point = (bucket_start: string, p95_ms: number, count = 10, error_count = 0) => ({
  bucket_start,
  count,
  error_count,
  avg_ms: p95_ms / 2,
  p50_ms: p95_ms / 2,
  p95_ms,
  p99_ms: p95_ms * 2,
})

describe('parseTrendDays', () => {
  test('accepts whole days up to the maximum', () => {
    expect(parseTrendDays('14')).toEqual({ ok: true, days: 14 })
    expect(parseTrendDays(String(MAX_TREND_DAYS))).toEqual({ ok: true, days: MAX_TREND_DAYS })
  })

  test('rejects zero, fractions, junk and too many days, naming the flag', () => {
    for (const bad of ['0', '1.5', 'week', String(MAX_TREND_DAYS + 1)]) {
      const result = parseTrendDays(bad)
      expect(result.ok).toBe(false)
      if (!result.ok) expect(result.error).toContain('--days')
    }
  })
})

describe('summarizeByDay', () => {
  test('groups hours by UTC day, sums calls and takes the median hourly p95', () => {
    const rows = summarizeByDay([
      point('2026-11-20T01:00:00Z', 300, 5, 1),
      point('2026-11-19T10:00:00Z', 100),
      point('2026-11-19T11:00:00Z', 200),
      point('2026-11-19T12:00:00Z', 900),
    ])
    expect(rows.map((r) => r.day)).toEqual(['2026-11-19', '2026-11-20'])
    expect(rows[0]).toEqual({ day: '2026-11-19', calls: 30, errors: 0, p95Ms: 200, hours: 3 })
    expect(rows[1]!.errors).toBe(1)
  })

  test('returns nothing for no points', () => {
    expect(summarizeByDay([])).toEqual([])
  })
})

describe('formatChange', () => {
  test('signs and rounds the change', () => {
    expect(formatChange(0.404)).toBe('+40%')
    expect(formatChange(-0.3)).toBe('-30%')
    expect(formatChange(null)).toBe('-')
  })
})

describe('formatSharePercent', () => {
  test('rounds to whole percents and keeps tiny shares visible', () => {
    expect(formatSharePercent(0.756)).toBe('76%')
    expect(formatSharePercent(0.002)).toBe('<1%')
    expect(formatSharePercent(1.4)).toBe('140%')
    expect(formatSharePercent(null)).toBe('-')
  })
})
