import { describe, expect, test } from 'bun:test'
import {
  parseSentryTimestamp,
  sentryTimestampToMillis,
} from './sentry-timestamp'

describe('parseSentryTimestamp', () => {
  test('parses numeric Unix timestamps as seconds', () => {
    const parsed = parseSentryTimestamp(1787071003.712074)

    expect(parsed?.toISOString()).toBe('2026-08-18T16:36:43.712Z')
  })

  test('parses RFC3339 timestamps with offset and nanosecond precision', () => {
    const parsed = parseSentryTimestamp(
      '2026-08-19T00:36:43.712074786+08:00'
    )

    expect(parsed?.toISOString()).toBe('2026-08-18T16:36:43.712Z')
  })

  test('assumes UTC when an RFC3339 timestamp omits a timezone', () => {
    const parsed = parseSentryTimestamp('2011-05-02T17:41:36.000')

    expect(parsed?.toISOString()).toBe('2011-05-02T17:41:36.000Z')
  })

  test('returns null for invalid timestamps', () => {
    expect(parseSentryTimestamp('not-a-timestamp')).toBeNull()
    expect(parseSentryTimestamp(Number.NaN)).toBeNull()
    expect(parseSentryTimestamp(Number.POSITIVE_INFINITY)).toBeNull()
    expect(parseSentryTimestamp('')).toBeNull()
  })

  test('does not reinterpret numeric strings as protocol timestamps', () => {
    expect(parseSentryTimestamp('1787071003.712074')).toBeNull()
  })

  test('returns milliseconds for timestamp comparisons and durations', () => {
    expect(
      sentryTimestampToMillis('2026-08-19T00:36:43.712074786+08:00')
    ).toBe(1787071003712)
  })
})
