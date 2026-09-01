// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseValue, assertValueType, formatValue, effectiveValue } from './index.js'
import type { FlagResponse } from '../../api/types.gen.js'

function makeFlag(overrides: Partial<FlagResponse> = {}): FlagResponse {
  return {
    id: 1,
    key: 'my.flag',
    value_type: 'bool',
    default_value: false,
    client_visible: false,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    environments: [],
    ...overrides,
  }
}

describe('assertValueType', () => {
  test('accepts each known value type', () => {
    expect(assertValueType('bool')).toBe('bool')
    expect(assertValueType('string')).toBe('string')
    expect(assertValueType('number')).toBe('number')
    expect(assertValueType('json')).toBe('json')
  })

  test('rejects anything else, naming the allowed set', () => {
    expect(() => assertValueType('int')).toThrow(/bool, string, number, json/)
  })
})

describe('parseValue', () => {
  describe('bool', () => {
    test('parses "true" and "false"', () => {
      expect(parseValue('true', 'bool', 'k')).toBe(true)
      expect(parseValue('false', 'bool', 'k')).toBe(false)
    })

    test('rejects anything else, naming the flag', () => {
      expect(() => parseValue('yes', 'bool', 'my.flag')).toThrow(/"my.flag" is a bool/)
    })
  })

  describe('number', () => {
    test('parses a numeric string', () => {
      expect(parseValue('200', 'number', 'k')).toBe(200)
      expect(parseValue('-3.5', 'number', 'k')).toBe(-3.5)
    })

    test('rejects non-finite input', () => {
      expect(() => parseValue('abc', 'number', 'worker.batch_size')).toThrow(
        /"worker.batch_size" is a number/,
      )
    })

    test('rejects Infinity, which Number() would otherwise accept', () => {
      expect(() => parseValue('Infinity', 'number', 'k')).toThrow(/could not parse/)
    })
  })

  describe('string', () => {
    test('returns the raw string untouched, including whitespace', () => {
      expect(parseValue(' raw value ', 'string', 'k')).toBe(' raw value ')
    })
  })

  describe('json', () => {
    test('parses valid JSON', () => {
      expect(parseValue('{"a":1}', 'json', 'k')).toEqual({ a: 1 })
      expect(parseValue('[1,2,3]', 'json', 'k')).toEqual([1, 2, 3])
    })

    test('rejects invalid JSON, naming the flag', () => {
      expect(() => parseValue('{oops}', 'json', 'my.flag')).toThrow(/"my.flag" is json/)
    })
  })

  test('rejects an unknown declared type', () => {
    expect(() => parseValue('x', 'enum' as never, 'my.flag')).toThrow(/Unknown value type "enum"/)
  })
})

describe('formatValue', () => {
  test('renders null and undefined as inheriting the default, not blank', () => {
    expect(formatValue(null)).toContain('(inherits default)')
    expect(formatValue(undefined)).toContain('(inherits default)')
  })

  test('returns strings as-is, without quoting', () => {
    expect(formatValue('hello')).toBe('hello')
  })

  test('serialises non-string values as JSON', () => {
    expect(formatValue(42)).toBe('42')
    expect(formatValue(true)).toBe('true')
    expect(formatValue({ a: 1 })).toBe('{"a":1}')
  })
})

describe('effectiveValue', () => {
  test('falls back to the default, marked, when there is no override for the environment', () => {
    const flag = makeFlag({ default_value: 'fallback', environments: [] })
    expect(effectiveValue(flag, 7)).toContain('fallback')
    expect(effectiveValue(flag, 7)).toContain('(default)')
  })

  test('shows the default, marked disabled, when the override is a kill switch', () => {
    const flag = makeFlag({
      default_value: 'fallback',
      environments: [{ environment_id: 7, enabled: false, value: 'overridden' }],
    })
    const rendered = effectiveValue(flag, 7)
    expect(rendered).toContain('fallback')
    expect(rendered).toContain('(disabled)')
    // The disabled override's own value must never leak through.
    expect(rendered).not.toContain('overridden')
  })

  test('falls back to the default when the override is enabled but has no value set', () => {
    const flag = makeFlag({
      default_value: 'fallback',
      environments: [{ environment_id: 7, enabled: true, value: null }],
    })
    expect(effectiveValue(flag, 7)).toContain('fallback')
    expect(effectiveValue(flag, 7)).toContain('(default)')
  })

  test('renders the override value when the override is enabled and set', () => {
    const flag = makeFlag({
      default_value: 'fallback',
      environments: [{ environment_id: 7, enabled: true, value: 'overridden' }],
    })
    expect(effectiveValue(flag, 7)).toBe('overridden')
  })

  test('matches by environment_id, ignoring overrides for other environments', () => {
    const flag = makeFlag({
      default_value: 'fallback',
      environments: [{ environment_id: 99, enabled: true, value: 'other-env' }],
    })
    expect(effectiveValue(flag, 7)).toContain('fallback')
  })
})
