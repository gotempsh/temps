// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { truncateKey } from './index.js'

describe('truncateKey', () => {
  test('leaves short keys untouched', () => {
    expect(truncateKey('short')).toBe('short')
  })

  test('leaves keys exactly at the 12-char threshold untouched', () => {
    const key = 'a'.repeat(12)
    expect(truncateKey(key)).toBe(key)
  })

  test('truncates long keys to first 8 + ... + last 4 chars', () => {
    const key = 'pk_live_abcdefghijklmnopqrstuvwxyz'
    expect(truncateKey(key)).toBe('pk_live_...wxyz')
  })

  test('never grows the string, only shrinks it', () => {
    const key = 'x'.repeat(64)
    const out = truncateKey(key)
    expect(out.length).toBeLessThan(key.length)
    expect(out).toBe('xxxxxxxx...xxxx')
  })
})
