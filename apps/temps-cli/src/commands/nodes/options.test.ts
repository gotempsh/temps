// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseId, parsePortOption, validPort } from './options.js'

describe('parseId', () => {
  test('reads a positive integer written with digits only', () => {
    expect(parseId('12')).toBe(12)
    expect(parseId(' 7 ')).toBe(7)
  })

  test('refuses anything else instead of reading a different id', () => {
    for (const value of [
      '12abc',
      '1.5',
      '-3',
      '0x10',
      '1e3',
      '',
      ' ',
      '0',
      '99999999999999999999',
    ]) {
      expect(parseId(value)).toBeNull()
    }
  })
})

describe('ports', () => {
  test('a port option is digits only', () => {
    expect(parsePortOption('2222')).toBe(2222)
    expect(parsePortOption('22abc')).toBeNaN()
    expect(parsePortOption('-1')).toBeNaN()
  })

  test('a valid port is unset or 1 to 65535', () => {
    expect(validPort(undefined)).toBe(true)
    expect(validPort(1)).toBe(true)
    expect(validPort(65535)).toBe(true)
    expect(validPort(0)).toBe(false)
    expect(validPort(65536)).toBe(false)
    expect(validPort(Number.NaN)).toBe(false)
    expect(validPort(parsePortOption('22abc'))).toBe(false)
  })
})
