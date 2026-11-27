// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  bucketParts,
  bucketRangeLabel,
  bucketSummary,
  formatDurationShort,
  formatUptime,
  unknownTimeNote,
} from './monitor-uptime'

describe('bucketParts', () => {
  test('a recovery inside a degraded hour keeps its successful checks', () => {
    // The reported hour: 5 successful and 36 degraded checks drew all yellow.
    const parts = bucketParts({
      operational_count: 5,
      degraded_count: 36,
      down_count: 0,
    })
    expect(parts.map((part) => [part.tone, part.count])).toEqual([
      ['operational', 5],
      ['degraded', 36],
    ])
    expect(parts[0].share).toBeCloseTo(5 / 41)
    expect(parts[1].share).toBeCloseTo(36 / 41)
  })

  test('successful first, then degraded, then failed; empty parts left out', () => {
    expect(
      bucketParts({
        operational_count: 1,
        degraded_count: 0,
        down_count: 2,
      }).map((part) => part.tone)
    ).toEqual(['operational', 'down'])
  })

  test('a bucket no check landed in has no parts, so it draws grey', () => {
    expect(
      bucketParts({ operational_count: 0, degraded_count: 0, down_count: 0 })
    ).toEqual([])
    expect(bucketParts({})).toEqual([])
  })

  test('shares add up to the whole bucket', () => {
    const parts = bucketParts({
      operational_count: 3,
      degraded_count: 4,
      down_count: 5,
    })
    expect(parts.reduce((sum, part) => sum + part.share, 0)).toBeCloseTo(1)
  })
})

describe('bucketSummary', () => {
  test('names every kind of check, so the counts add up', () => {
    expect(
      bucketSummary(
        bucketParts({ operational_count: 5, degraded_count: 36, down_count: 0 })
      )
    ).toBe('5 successful, 36 degraded of 41 checks')
  })

  test('checks that agree are said once', () => {
    expect(
      bucketSummary(
        bucketParts({ operational_count: 0, degraded_count: 49, down_count: 0 })
      )
    ).toBe('49 checks, all degraded')
    expect(
      bucketSummary(
        bucketParts({ operational_count: 1, degraded_count: 0, down_count: 0 })
      )
    ).toBe('1 successful check')
  })

  test('a bucket nothing checked says so', () => {
    expect(bucketSummary([])).toBe('No checks')
  })
})

describe('bucketRangeLabel', () => {
  // The label is in the reader's zone: the expectation is computed the same
  // way, so the test passes in any zone.
  const start = '2026-11-27T20:00:00Z'
  const local = (iso: string) => new Date(iso)

  test('an hourly bucket names its hour', () => {
    const label = bucketRangeLabel(start, 'hourly', 'en-GB')
    const from = local(start).toLocaleTimeString('en-GB', {
      hour: '2-digit',
      minute: '2-digit',
      hourCycle: 'h23',
    })
    const to = local('2026-11-27T21:00:00Z').toLocaleTimeString('en-GB', {
      hour: '2-digit',
      minute: '2-digit',
      hourCycle: 'h23',
    })
    expect(label.endsWith(`${from}–${to}`)).toBe(true)
  })

  test('a daily bucket names its day alone', () => {
    expect(bucketRangeLabel('2026-11-27T12:00:00Z', 'daily', 'en-GB')).toBe(
      '27 Nov 2026'
    )
  })

  test('an unreadable start is shown as it came', () => {
    expect(bucketRangeLabel('not a date', 'hourly')).toBe('not a date')
  })
})

describe('formatDurationShort', () => {
  test('reads like an operator would say it', () => {
    expect(formatDurationShort(30)).toBe('30s')
    expect(formatDurationShort(45 * 60)).toBe('45m')
    expect(formatDurationShort(3 * 3600)).toBe('3h')
    expect(formatDurationShort(3 * 3600 - 120)).toBe('2h 58m')
    expect(formatDurationShort(3 * 86400 + 3600)).toBe('3d 1h')
  })
})

describe('formatUptime', () => {
  test('no monitored time is a dash, not 0% or 100%', () => {
    expect(formatUptime(null)).toBe('—')
    expect(formatUptime(undefined)).toBe('—')
    expect(formatUptime(87.5)).toBe('87.50%')
  })
})

describe('unknownTimeNote', () => {
  test('flags a three-hour gap in a day', () => {
    expect(
      unknownTimeNote({ monitored_seconds: 75_600, unknown_seconds: 10_800 })
    ).toBe('No checks for 3h (12.5% of the range)')
  })

  test('stays quiet when the range was monitored', () => {
    expect(
      unknownTimeNote({ monitored_seconds: 86_400, unknown_seconds: 0 })
    ).toBeNull()
    expect(
      unknownTimeNote({ monitored_seconds: 86_370, unknown_seconds: 30 })
    ).toBeNull()
  })

  test('a range nothing checked is the whole range', () => {
    expect(
      unknownTimeNote({ monitored_seconds: 0, unknown_seconds: 86_400 })
    ).toBe('No checks for 24h (the whole range)')
  })
})
