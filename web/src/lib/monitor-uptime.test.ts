// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  bucketTone,
  formatDurationShort,
  formatUptime,
  unknownTimeNote,
} from './monitor-uptime'

describe('bucketTone', () => {
  test('a bucket without checks is unknown, never operational', () => {
    expect(bucketTone('unknown')).toBe('unknown')
    expect(bucketTone(undefined)).toBe('unknown')
  })

  test('buckets say "down", checks say "major_outage": both are down', () => {
    expect(bucketTone('down')).toBe('down')
    expect(bucketTone('major_outage')).toBe('down')
    expect(bucketTone('partial_outage')).toBe('degraded')
    expect(bucketTone('degraded')).toBe('degraded')
    expect(bucketTone('operational')).toBe('operational')
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
