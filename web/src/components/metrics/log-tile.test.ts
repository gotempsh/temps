// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatLogTileMetric,
  isLogAttributeKey,
  logTileBucketSecs,
  logTileProblem,
  searchFiltersText,
  logTileMetricLabel,
  parseLogTileMetric,
} from './log-tile'

const from = '2026-01-01T00:00:00.000Z'
const at = (hours: number) =>
  new Date(Date.parse(from) + hours * 3_600_000).toISOString()

describe('log tile metric', () => {
  test('a count has no attribute', () => {
    expect(formatLogTileMetric('count', 'ignored')).toBe('count')
    expect(parseLogTileMetric('count')).toEqual({ fn: 'count', attribute: '' })
  })

  test('a numeric field round-trips', () => {
    const metric = formatLogTileMetric('p95', ' duration_ms ')
    expect(metric).toBe('p95:duration_ms')
    expect(parseLogTileMetric(metric)).toEqual({
      fn: 'p95',
      attribute: 'duration_ms',
    })
    expect(logTileMetricLabel(metric)).toBe('p95 duration_ms')
  })

  test('an unknown or missing metric reads as a count', () => {
    expect(parseLogTileMetric(undefined).fn).toBe('count')
    expect(parseLogTileMetric('median:x').fn).toBe('count')
    expect(parseLogTileMetric(':x').fn).toBe('count')
  })
})

describe('logTileBucketSecs', () => {
  test('a short window uses one-minute buckets', () => {
    expect(logTileBucketSecs(from, at(1), 'count')).toBe(60)
  })

  test('stays under 240 buckets for a count', () => {
    const secs = logTileBucketSecs(from, at(24), 'count')
    expect(secs).toBe(600)
    expect((24 * 3600) / secs).toBeLessThanOrEqual(240)
  })

  test('stays under 60 buckets for a numeric field', () => {
    const secs = logTileBucketSecs(from, at(24), 'avg:duration_ms')
    expect(secs).toBe(1800)
    expect((24 * 3600) / secs).toBeLessThanOrEqual(60)
  })

  test('a very long window falls back to whole days', () => {
    const secs = logTileBucketSecs(from, at(24 * 365), 'avg:x')
    expect(secs % 86400).toBe(0)
    expect((365 * 86400) / secs).toBeLessThanOrEqual(60)
  })
})

describe('isLogAttributeKey', () => {
  test('accepts what the line index accepts', () => {
    for (const key of [
      'duration_ms',
      'http.status',
      '_x',
      `a${'b'.repeat(63)}`,
    ])
      expect(isLogAttributeKey(key)).toBe(true)
  })

  test('refuses metric-name syntax the index refuses', () => {
    for (const key of [
      '',
      'request-time',
      '9lives',
      'a/b',
      'a:b',
      `a${'b'.repeat(64)}`,
    ])
      expect(isLogAttributeKey(key)).toBe(false)
  })
})

describe('logTileProblem', () => {
  test('a count charts any search', () => {
    expect(logTileProblem('count', true)).toBeNull()
  })

  test('a numeric field cannot chart a text-filtered search', () => {
    expect(logTileProblem('p95', true)).toContain('Matching lines')
    expect(logTileProblem('p95', false)).toBeNull()
  })

  test('only a non-empty text counts as a text filter', () => {
    expect(searchFiltersText({ text: 'payment failed' })).toBe(true)
    expect(searchFiltersText({ text: '' })).toBe(false)
    expect(searchFiltersText({ text: null })).toBe(false)
    expect(searchFiltersText(undefined)).toBe(false)
  })
})
