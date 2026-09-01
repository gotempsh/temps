// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { bucketSizeForRange, renderSparkline, SPARKLINE_WIDTH } from './overview.js'

function rangeOfHours(hours: number): { startDate: string; endDate: string } {
  return { startDate: new Date(0).toISOString(), endDate: new Date(hours * 3_600_000).toISOString() }
}

describe('bucketSizeForRange', () => {
  // Regression coverage: this function exists specifically so `--period 7d`
  // and `--period 30d` don't silently render the same last-two-days
  // sparkline under a header claiming a longer range (see the code comment).
  test('picks hourly buckets up to the sparkline width', () => {
    const { startDate, endDate } = rangeOfHours(SPARKLINE_WIDTH)
    expect(bucketSizeForRange(startDate, endDate)).toBe('1 hour')
  })

  test('steps up to 6-hour buckets just past the hourly range', () => {
    const { startDate, endDate } = rangeOfHours(SPARKLINE_WIDTH + 1)
    expect(bucketSizeForRange(startDate, endDate)).toBe('6 hours')
  })

  test('steps up to daily buckets past the 6-hour range', () => {
    const { startDate, endDate } = rangeOfHours(SPARKLINE_WIDTH * 6 + 1)
    expect(bucketSizeForRange(startDate, endDate)).toBe('1 day')
  })

  test('steps up to weekly buckets past the daily range', () => {
    const { startDate, endDate } = rangeOfHours(SPARKLINE_WIDTH * 24 + 1)
    expect(bucketSizeForRange(startDate, endDate)).toBe('1 week')
  })

  test('falls back to monthly buckets past the weekly range', () => {
    const { startDate, endDate } = rangeOfHours(SPARKLINE_WIDTH * 24 * 7 + 1)
    expect(bucketSizeForRange(startDate, endDate)).toBe('1 month')
  })
})

describe('renderSparkline', () => {
  test('renders an empty series as an empty string, not a crash', () => {
    // Math.max(...[].map(...), 1) guards against Math.max() === -Infinity.
    expect(renderSparkline([])).toBe('')
  })

  test('maps counts to block characters relative to the series max', () => {
    expect(renderSparkline([{ count: 0 }, { count: 50 }, { count: 100 }])).toBe('▁▄█')
  })

  test('keeps only the most recent SPARKLINE_WIDTH points', () => {
    const points = Array.from({ length: 50 }, (_, i) => ({ count: i }))
    const out = renderSparkline(points)
    expect(out.length).toBe(SPARKLINE_WIDTH)
    // Dropped the two oldest (lowest-count) points, so the line starts on the
    // lowest surviving block, not the absolute lowest block.
    expect(out.startsWith('▁')).toBe(true)
  })

  test('a flat series maxes out every block', () => {
    expect(renderSparkline([{ count: 5 }, { count: 5 }])).toBe('██')
  })
})
