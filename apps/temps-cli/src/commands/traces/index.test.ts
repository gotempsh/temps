import { test, expect, describe } from 'bun:test'
import { parseSince, formatMs, formatTotal, resolveTimeWindow, MAX_WINDOW_DAYS } from './index.js'

describe('parseSince', () => {
  test('parses minutes, hours, and days', () => {
    expect(parseSince('30m')).toBe(30 * 60_000)
    expect(parseSince('24h')).toBe(24 * 3_600_000)
    expect(parseSince('7d')).toBe(7 * 86_400_000)
  })

  test('is case-insensitive and tolerates surrounding whitespace', () => {
    expect(parseSince('2H')).toBe(2 * 3_600_000)
    expect(parseSince(' 5m ')).toBe(5 * 60_000)
  })

  test('rejects shapes that are not <number><unit>', () => {
    expect(parseSince('abc')).toBeNull()
    expect(parseSince('5x')).toBeNull()
    expect(parseSince('')).toBeNull()
    expect(parseSince('h5')).toBeNull()
  })
})

describe('formatMs', () => {
  test('renders sub-millisecond and sub-second durations with more precision', () => {
    expect(formatMs(0.5)).toBe('0.50ms')
    expect(formatMs(500)).toBe('500.0ms')
  })

  test('switches to seconds at the 1000ms boundary', () => {
    expect(formatMs(999)).toBe('999.0ms')
    expect(formatMs(1000)).toBe('1.00s')
    expect(formatMs(1500)).toBe('1.50s')
  })
})

describe('formatTotal', () => {
  test('falls back to formatMs under a minute', () => {
    expect(formatTotal(500)).toBe('500.0ms')
  })

  test('switches to minutes at the 60s boundary', () => {
    expect(formatTotal(60_000)).toBe('1.0m')
    expect(formatTotal(90_000)).toBe('1.5m')
  })

  test('switches to hours at the 60m boundary', () => {
    expect(formatTotal(3_600_000)).toBe('1.00h')
    expect(formatTotal(7_200_000)).toBe('2.00h')
  })
})

describe('resolveTimeWindow', () => {
  test('derives startTime from --since relative to --end-time', () => {
    const result = resolveTimeWindow({ since: '24h', endTime: '2024-01-02T00:00:00.000Z' })
    expect(result.ok).toBe(true)
    if (!result.ok) throw new Error('expected ok result')
    expect(result.startTime.toISOString()).toBe('2024-01-01T00:00:00.000Z')
    expect(result.endTime.toISOString()).toBe('2024-01-02T00:00:00.000Z')
  })

  test('--start-time overrides --since', () => {
    const result = resolveTimeWindow({
      since: '1h',
      startTime: '2024-01-01T00:00:00.000Z',
      endTime: '2024-01-02T00:00:00.000Z',
    })
    expect(result.ok).toBe(true)
    if (!result.ok) throw new Error('expected ok result')
    expect(result.startTime.toISOString()).toBe('2024-01-01T00:00:00.000Z')
  })

  test('rejects an unparsable --end-time', () => {
    const result = resolveTimeWindow({ endTime: 'not-a-date' })
    expect(result).toEqual({ ok: false, error: 'Invalid --end-time: not-a-date' })
  })

  test('rejects an unparsable --start-time', () => {
    const result = resolveTimeWindow({ startTime: 'nope', endTime: '2024-01-02T00:00:00.000Z' })
    expect(result).toEqual({ ok: false, error: 'Invalid --start-time: nope' })
  })

  test('rejects a --since that does not match <number><unit>', () => {
    const result = resolveTimeWindow({ since: 'nonsense', endTime: '2024-01-02T00:00:00.000Z' })
    expect(result.ok).toBe(false)
    if (result.ok) throw new Error('expected error result')
    expect(result.error).toMatch(/Invalid --since: nonsense/)
  })

  test('accepts a window exactly at MAX_WINDOW_DAYS', () => {
    const result = resolveTimeWindow({
      startTime: '2024-01-01T00:00:00.000Z',
      endTime: new Date(
        new Date('2024-01-01T00:00:00.000Z').getTime() + MAX_WINDOW_DAYS * 86_400_000,
      ).toISOString(),
    })
    expect(result.ok).toBe(true)
  })

  test('rejects a window wider than MAX_WINDOW_DAYS, naming the flags to fix', () => {
    // The server rejects an over-wide window rather than narrowing it, so the
    // CLI needs to catch it locally and point at the flag to change.
    const result = resolveTimeWindow({
      startTime: '2024-01-01T00:00:00.000Z',
      endTime: '2024-03-01T00:00:00.000Z',
    })
    expect(result.ok).toBe(false)
    if (result.ok) throw new Error('expected error result')
    expect(result.error).toContain(`the maximum is ${MAX_WINDOW_DAYS}`)
    expect(result.error).toContain('--start-time/--end-time')
  })
})
