// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  committedNumber,
  firstNumericError,
  formatNumericText,
  parseNumericText,
  isEchoOfOwnEdit,
  textAfterOwnEdit,
  shouldResyncNumericText,
  validateNumeric,
} from './numeric-input'

describe('parseNumericText', () => {
  test('empty and blank text is an allowed transient state', () => {
    expect(parseNumericText('')).toEqual({ kind: 'empty' })
    expect(parseNumericText('   ')).toEqual({ kind: 'empty' })
  })

  test('parses integers, decimals, negatives and leading zeros', () => {
    expect(parseNumericText('42')).toEqual({ kind: 'number', value: 42 })
    expect(parseNumericText('0.5')).toEqual({ kind: 'number', value: 0.5 })
    expect(parseNumericText('-3')).toEqual({ kind: 'number', value: -3 })
    expect(parseNumericText('042')).toEqual({ kind: 'number', value: 42 })
  })

  test('text that is not a number is invalid, never coerced to 0', () => {
    expect(parseNumericText('abc')).toEqual({ kind: 'invalid' })
    expect(parseNumericText('-')).toEqual({ kind: 'invalid' })
    expect(parseNumericText('Infinity')).toEqual({ kind: 'invalid' })
  })
})

describe('editing a field that was emptied', () => {
  // The reported bug: backspacing the field snapped it back to its default,
  // so typing "42" afterwards produced "042" (or "6042").
  test('clearing commits "no value", not a default', () => {
    expect(committedNumber('')).toBeUndefined()
  })

  test('typing 42 after clearing commits 4 then 42', () => {
    const commits = ['', '4', '42'].map(committedNumber)
    expect(commits).toEqual([undefined, 4, 42])
  })

  test('non-numeric text commits nothing so the previous value stands', () => {
    expect(committedNumber('-')).toBeNull()
  })

  test('a committed value round-trips to the text shown', () => {
    expect(formatNumericText(42)).toBe('42')
    expect(formatNumericText(0)).toBe('0')
    expect(formatNumericText(undefined)).toBe('')
    expect(formatNumericText(null)).toBe('')
    expect(formatNumericText(Number.NaN)).toBe('')
  })
})

describe('validateNumeric (submit-time rules)', () => {
  test('accepts values inside the range, bounds included', () => {
    expect(validateNumeric(1, { min: 1, max: 10 })).toBeNull()
    expect(validateNumeric(10, { min: 1, max: 10 })).toBeNull()
    expect(validateNumeric(0, { min: 0 })).toBeNull()
  })

  test('rejects below min and above max', () => {
    expect(validateNumeric(0, { min: 1, max: 10 })).toBe(
      'Must be between 1 and 10'
    )
    expect(validateNumeric(11, { min: 1, max: 10 })).toBe(
      'Must be between 1 and 10'
    )
    expect(validateNumeric(-1, { min: 0 })).toBe('Must be at least 0')
    expect(validateNumeric(101, { max: 100 })).toBe('Must be at most 100')
  })

  test('an empty field fails when required, passes when optional', () => {
    expect(validateNumeric(undefined, { min: 1 })).toBe('Enter a number')
    expect(validateNumeric(null)).toBe('Enter a number')
    expect(validateNumeric(Number.NaN)).toBe('Enter a number')
    expect(validateNumeric(undefined, { required: false })).toBeNull()
  })

  test('zero is a value, not an empty field', () => {
    expect(validateNumeric(0, { min: 0, required: true })).toBeNull()
  })

  test('non-integers are rejected only for integer fields', () => {
    expect(validateNumeric(1.5, { integer: true })).toBe('Enter a whole number')
    expect(validateNumeric(1.5)).toBeNull()
    expect(validateNumeric(2, { integer: true })).toBeNull()
  })

  test('infinite values are rejected', () => {
    expect(validateNumeric(Number.POSITIVE_INFINITY)).toBe('Enter a number')
  })
})

describe('firstNumericError', () => {
  test('names the first failing field', () => {
    expect(
      firstNumericError([
        { label: 'Max turns', value: 5, rules: { min: 1 } },
        { label: 'Timeout', value: 0, rules: { min: 1 } },
        { label: 'Budget', value: undefined },
      ])
    ).toBe('Timeout: Must be at least 1')
  })

  test('is null when every field is valid', () => {
    expect(
      firstNumericError([{ label: 'Port', value: 8080, rules: { max: 65535 } }])
    ).toBeNull()
  })
})

describe('shouldResyncNumericText', () => {
  test('leaves text that already means the committed value', () => {
    expect(shouldResyncNumericText('42', 42)).toBe(false)
    expect(shouldResyncNumericText('042', 42)).toBe(false)
    expect(shouldResyncNumericText('', undefined)).toBe(false)
    expect(shouldResyncNumericText('', null)).toBe(false)
  })

  test('follows a value changed from outside', () => {
    expect(shouldResyncNumericText('42', 7)).toBe(true)
    expect(shouldResyncNumericText('', 7)).toBe(true)
    expect(shouldResyncNumericText('7', undefined)).toBe(true)
  })

  test('does not treat an emptied field as 0', () => {
    expect(shouldResyncNumericText('', 0)).toBe(true)
  })

  // react-hook-form answers a field set to `undefined` with the field's
  // `defaultValues` entry, so the value that comes back after emptying a
  // field is the default (60), not what was committed. Following it put the
  // default straight back and turned "clear, type 42" into "6042".
  test("ignores the parent answering this field's own edit", () => {
    expect(shouldResyncNumericText('', 60)).toBe(true)
    expect(shouldResyncNumericText('', 60, true)).toBe(false)
    expect(shouldResyncNumericText('4', 60, true)).toBe(false)
  })
})

describe('isEchoOfOwnEdit', () => {
  test('any answer to an emptied field is the form falling back, not a reset', () => {
    expect(isEchoOfOwnEdit(undefined, 60)).toBe(true)
    expect(isEchoOfOwnEdit(undefined, undefined)).toBe(true)
  })

  test('the same number coming back is an echo', () => {
    expect(isEchoOfOwnEdit(42, 42)).toBe(true)
  })

  // A parent that clamps (50 max) answers a typed 100 with 50: the field has
  // to show 50, or it displays a value the app is not using.
  test('a different number is the parent overriding the edit', () => {
    expect(isEchoOfOwnEdit(100, 50)).toBe(false)
    expect(isEchoOfOwnEdit(-5, undefined)).toBe(false)
  })
})

describe('textAfterOwnEdit', () => {
  test('shows the clamped value even though the parent value did not change', () => {
    // contextLines is 50; the user types "500"; the parent keeps 50.
    expect(textAfterOwnEdit('500', 500, 50)).toBe('50')
  })

  test('shows an emptied field when the parent turned the edit into "no value"', () => {
    expect(textAfterOwnEdit('-5', -5, undefined)).toBe('')
  })

  test('leaves text alone when the parent accepted the edit', () => {
    expect(textAfterOwnEdit('42', 42, 42)).toBeNull()
    expect(textAfterOwnEdit('042', 42, 42)).toBeNull()
    expect(textAfterOwnEdit('4', 4, 4)).toBeNull()
  })

  test('leaves an emptied field empty whatever the form falls back to', () => {
    expect(textAfterOwnEdit('', undefined, 60)).toBeNull()
    expect(textAfterOwnEdit('', undefined, undefined)).toBeNull()
  })
})
