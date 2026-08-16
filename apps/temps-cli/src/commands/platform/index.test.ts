import { test, expect, describe } from 'bun:test'
import {
  extractIpValue,
  isTerminalUpdateAttempt,
  parseUpdateChannel,
  buildSelfUpdateSettings,
} from './index.js'

describe('extractIpValue', () => {
  test('unwraps an { ip } object', () => {
    expect(extractIpValue({ ip: '10.0.0.1' })).toBe('10.0.0.1')
  })

  test('coerces a non-string ip field to a string', () => {
    // The generated SDK types the field as string, but defensively handle a
    // server that sends something else rather than printing "[object Object]".
    expect(extractIpValue({ ip: 12345 })).toBe('12345')
  })

  test('passes a bare string result through untouched', () => {
    expect(extractIpValue('203.0.113.9')).toBe('203.0.113.9')
  })

  test('returns null for a shape it does not recognise so the caller falls back to raw JSON', () => {
    expect(extractIpValue({ address: '10.0.0.1' })).toBeNull()
    expect(extractIpValue(null)).toBeNull()
    expect(extractIpValue(undefined)).toBeNull()
    expect(extractIpValue(42)).toBeNull()
  })
})

describe('isTerminalUpdateAttempt', () => {
  test('is false when there is no attempt at all', () => {
    expect(isTerminalUpdateAttempt(null, null)).toBe(false)
    expect(isTerminalUpdateAttempt(undefined, null)).toBe(false)
  })

  test('is false while the attempt is still pending', () => {
    expect(
      isTerminalUpdateAttempt({ status: 'pending', started_at: '2026-01-01T00:00:00Z' }, null)
    ).toBe(false)
  })

  test('is false when the attempt is a stale one from before this poll started', () => {
    // Guards against reporting a leftover failed/succeeded attempt as if it
    // were the outcome of the update we just kicked off.
    expect(
      isTerminalUpdateAttempt(
        { status: 'succeeded', started_at: '2026-01-01T00:00:00Z' },
        '2026-01-01T00:00:00Z'
      )
    ).toBe(false)
  })

  test('is true for a terminal attempt distinct from the baseline', () => {
    expect(
      isTerminalUpdateAttempt(
        { status: 'succeeded', started_at: '2026-01-02T00:00:00Z' },
        '2026-01-01T00:00:00Z'
      )
    ).toBe(true)
    expect(
      isTerminalUpdateAttempt(
        { status: 'failed', started_at: '2026-01-02T00:00:00Z' },
        '2026-01-01T00:00:00Z'
      )
    ).toBe(true)
  })
})

describe('parseUpdateChannel', () => {
  test('accepts and normalises the known channels', () => {
    expect(parseUpdateChannel('stable')).toEqual({ requested: 'stable', isAuto: false })
    expect(parseUpdateChannel(' Beta ')).toEqual({ requested: 'beta', isAuto: false })
    expect(parseUpdateChannel('NIGHTLY')).toEqual({ requested: 'nightly', isAuto: false })
  })

  test('recognises the auto sentinel', () => {
    expect(parseUpdateChannel('auto')).toEqual({ requested: 'auto', isAuto: true })
  })

  test('rejects an unknown channel and names the valid ones', () => {
    const result = parseUpdateChannel('canary')
    expect('error' in result).toBe(true)
    expect((result as { error: string }).error).toContain("Unknown channel 'canary'")
    expect((result as { error: string }).error).toContain('stable, beta, nightly')
  })
})

describe('buildSelfUpdateSettings', () => {
  test('defaults enabled to true when the current settings have none', () => {
    // Switching channel must never silently disable self-update as a
    // side effect of the read-modify-write.
    expect(buildSelfUpdateSettings({}, 'beta', false)).toEqual({
      enabled: true,
      channel: 'beta',
    })
  })

  test('preserves an explicit enabled=false', () => {
    expect(
      buildSelfUpdateSettings({ self_update: { enabled: false } }, 'beta', false)
    ).toEqual({ enabled: false, channel: 'beta' })
  })

  test('sends a null channel for auto instead of the literal string', () => {
    expect(buildSelfUpdateSettings({}, 'auto', true)).toEqual({
      enabled: true,
      channel: null,
    })
  })
})
