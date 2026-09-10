// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatBooleanValue, isKnownPreferenceKey, parsePreferenceValue } from './index.js'

describe('isKnownPreferenceKey', () => {
  test('accepts boolean, number and string preference keys', () => {
    expect(isKnownPreferenceKey('email_enabled')).toBe(true)
    expect(isKnownPreferenceKey('error_threshold')).toBe(true)
    expect(isKnownPreferenceKey('minimum_severity')).toBe(true)
  })

  test('rejects a key that is not on the schema', () => {
    // Prevents silently merging an unknown field into the update payload.
    expect(isKnownPreferenceKey('not_a_real_key')).toBe(false)
  })
})

describe('parsePreferenceValue', () => {
  test('parses "true"/"false" for boolean keys', () => {
    expect(parsePreferenceValue('email_enabled', 'true')).toBe(true)
    expect(parsePreferenceValue('email_enabled', 'false')).toBe(false)
  })

  test('rejects a non-boolean value for a boolean key', () => {
    // A bad request here would otherwise reach the API as `undefined` or NaN.
    expect(() => parsePreferenceValue('email_enabled', 'yes')).toThrow(
      /Expected "true" or "false"/,
    )
  })

  test('parses numeric values for number keys', () => {
    expect(parsePreferenceValue('error_threshold', '10')).toBe(10)
    expect(parsePreferenceValue('ssl_days_before_expiration', '30')).toBe(30)
  })

  test('rejects a non-numeric value for a number key', () => {
    expect(() => parsePreferenceValue('error_threshold', 'abc')).toThrow(
      /Expected a number/,
    )
  })

  test('passes string keys through untouched', () => {
    expect(parsePreferenceValue('minimum_severity', 'critical')).toBe('critical')
    expect(parsePreferenceValue('digest_send_day', 'monday')).toBe('monday')
  })
})

describe('formatBooleanValue', () => {
  test('renders distinct text for enabled vs disabled', () => {
    expect(formatBooleanValue(true)).toContain('enabled')
    expect(formatBooleanValue(false)).toContain('disabled')
  })
})
