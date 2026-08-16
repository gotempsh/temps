import { test, expect, describe } from 'bun:test'
import { scanStatusColor, severityColor, truncate } from './index.js'

describe('scanStatusColor', () => {
  test('maps known statuses to their badge text', () => {
    expect(scanStatusColor('completed')).toBe('● success')
    expect(scanStatusColor('running')).toBe('● running')
    expect(scanStatusColor('in_progress')).toBe('● running')
    expect(scanStatusColor('pending')).toBe('● pending')
    expect(scanStatusColor('queued')).toBe('● pending')
    expect(scanStatusColor('failed')).toBe('● failed')
    expect(scanStatusColor('error')).toBe('● failed')
  })

  test('matching is case-insensitive', () => {
    expect(scanStatusColor('COMPLETED')).toBe('● success')
    expect(scanStatusColor('Failed')).toBe('● failed')
  })

  test('an unrecognized status is returned verbatim, not badged', () => {
    // Falls through the switch on the lowercased value but returns the
    // original (un-lowercased) string, so callers see exactly what the
    // server sent rather than a normalized/badged version.
    expect(scanStatusColor('Weird')).toBe('Weird')
  })
})

describe('severityColor', () => {
  test('maps known severities through to the colorizer, preserving original casing', () => {
    // The switch matches on severity.toUpperCase(), but the colorizer is
    // called with the original `severity` argument — so casing from the API
    // response passes through unchanged even though matching is
    // case-insensitive.
    expect(severityColor('CRITICAL')).toBe('CRITICAL')
    expect(severityColor('critical')).toBe('critical')
    expect(severityColor('HIGH')).toBe('HIGH')
    expect(severityColor('MEDIUM')).toBe('MEDIUM')
    expect(severityColor('LOW')).toBe('LOW')
  })

  test('an unrecognized severity is returned unchanged', () => {
    expect(severityColor('UNKNOWN')).toBe('UNKNOWN')
  })
})

describe('truncate', () => {
  test('leaves strings at or under the limit intact', () => {
    expect(truncate('short', 40)).toBe('short')
    expect(truncate('exactly8', 8)).toBe('exactly8')
  })

  test('truncates longer strings and appends an ellipsis within maxLength', () => {
    const out = truncate('x'.repeat(20), 10)
    expect(out).toBe('xxxxxxx...')
    expect(out.length).toBe(10)
  })
})
