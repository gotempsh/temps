// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { validateLoopbackJa4Header } from './loopback-ja4-header'

describe('validateLoopbackJa4Header', () => {
  test('accepts blank and header names containing ja4', () => {
    for (const value of ['', '  ', 'x-ja4', ' X-JA4 ', 'X-Client-Ja4']) {
      expect(validateLoopbackJa4Header(value)).toBe(true)
    }
  })

  test('refuses names that are not tokens or do not contain ja4', () => {
    expect(validateLoopbackJa4Header('x ja4')).toContain('valid header name')
    expect(validateLoopbackJa4Header('x-ja4:')).toContain('valid header name')
    expect(validateLoopbackJa4Header('authorization')).toContain('"ja4"')
  })
})
