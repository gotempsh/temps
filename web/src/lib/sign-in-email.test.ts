// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { signInEmailSchema } from '@/lib/sign-in-email'

describe('signInEmailSchema', () => {
  test('accepts the local administrator that temps start creates', () => {
    expect(signInEmailSchema.safeParse('admin@localhost').success).toBe(true)
  })

  test('accepts ordinary addresses and trims surrounding whitespace', () => {
    expect(signInEmailSchema.parse('  user@example.com ')).toBe(
      'user@example.com'
    )
    expect(
      signInEmailSchema.safeParse('first.last+tag@sub.example.co.uk').success
    ).toBe(true)
  })

  test('rejects an empty field with its own message', () => {
    const result = signInEmailSchema.safeParse('   ')
    expect(result.success).toBe(false)
    expect(result.error?.issues[0]?.message).toBe('Enter your email address')
  })

  test('rejects obvious typos', () => {
    for (const value of [
      'admin',
      'admin@',
      '@localhost',
      'ad min@localhost',
      'a@b@c',
    ]) {
      const result = signInEmailSchema.safeParse(value)
      expect(result.success).toBe(false)
      expect(result.error?.issues[0]?.message).toBe(
        'Please enter a valid email address'
      )
    }
  })
})
