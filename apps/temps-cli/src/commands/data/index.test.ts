import { test, expect, describe } from 'bun:test'
import { cell, formatBytes, validateFilter } from './index.js'

describe('validateFilter', () => {
  test('passes valid JSON through untouched', () => {
    const raw = '{"where":"id > 5"}'
    expect(validateFilter(raw, 'my-db')).toBe(raw)
  })

  test('returns undefined when no filter was given', () => {
    // Distinct from "filter that matches nothing" — the caller omitted it.
    expect(validateFilter(undefined, 'my-db')).toBeUndefined()
  })

  test('rejects a bare SQL WHERE clause rather than sending it', () => {
    // The most likely mistake: typing SQL directly instead of the JSON
    // envelope. Sending it would let the server drop the filter and return a
    // full unfiltered table, which reads as a correct answer.
    expect(() => validateFilter("plan = 'pro'", 'my-db')).toThrow(
      /--filter must be valid JSON/,
    )
  })

  test('names the service in the error so the fix is one command away', () => {
    expect(() => validateFilter('nonsense', 'cadence-demo-db')).toThrow(
      /temps data info cadence-demo-db/,
    )
  })

  test('echoes what was received so a typo is visible', () => {
    expect(() => validateFilter('{oops}', 'my-db')).toThrow(/\{oops\}/)
  })

  test('does not expose parser or terminal controls for a malicious filter', () => {
    const payload =
      '{"where":"\x1b]52;c;dG9rX2xpdmVfc2VjcmV0\x07\x1b[31m\n\u202E"'
    let message = ''
    try {
      validateFilter(payload, 'service\x1b]8;;https://attacker.example\x1b\\')
    } catch (error) {
      message = (error as Error).message
    }

    expect(message).toContain('--filter must be valid JSON')
    expect(message).not.toContain('\x1b')
    expect(message).not.toContain('\x07')
    expect(message).not.toContain('\n')
    expect(message).not.toContain('\u202E')
    expect(message).not.toContain('Unexpected')
  })
})

describe('cell', () => {
  test('renders null and undefined as a marker, not empty space', () => {
    // A blank cell is ambiguous: missing column vs NULL value.
    expect(cell(null)).toContain('null')
    expect(cell(undefined)).toContain('null')
  })

  test('serialises objects rather than printing [object Object]', () => {
    expect(cell({ a: 1 })).toBe('{"a":1}')
  })

  test('truncates long values so one blob cannot destroy the table', () => {
    const long = 'x'.repeat(100)
    const out = cell(long, 40)
    expect(out.length).toBe(40)
    expect(out.endsWith('…')).toBe(true)
  })

  test('leaves values at or under the limit intact', () => {
    expect(cell('short', 40)).toBe('short')
  })

  test('keeps numbers and booleans readable', () => {
    expect(cell(0)).toBe('0')
    expect(cell(false)).toBe('false')
  })

  test('strips terminal control sequences before truncating', () => {
    const malicious =
      '\x1b[31mred\x1b[0m\x1b]8;;https://attacker.example\x1b\\link\x1b]8;;\x1b\\' +
      '\x1b]52;c;dG9rX2xpdmVfc2VjcmV0\x07\nforged'
    const out = cell(malicious, 80)
    expect(out).toBe('redlink forged')
    expect(out).not.toContain('\x1b')
    expect(out).not.toContain('\n')
  })
})

describe('formatBytes', () => {
  test('scales through units', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(2048)).toBe('2 KB')
    expect(formatBytes(1024 * 1024 * 3)).toBe('3 MB')
  })

  test('renders a missing size as a dash, not 0 B', () => {
    // Many backends report no size at all; claiming "0 B" would be a lie.
    expect(formatBytes(null)).toContain('—')
    expect(formatBytes(undefined)).toContain('—')
  })

  test('keeps a real zero distinct from unknown', () => {
    expect(formatBytes(0)).toBe('0 B')
  })
})
