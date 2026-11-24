// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { currentUserEmailVerified } from './email-verification'

describe('currentUserEmailVerified', () => {
  test('reads the flag from the current-user response', () => {
    expect(currentUserEmailVerified({ id: 1, email_verified: false })).toBe(
      false
    )
    expect(currentUserEmailVerified({ id: 1, email_verified: true })).toBe(true)
  })

  test('treats a server without the field as verified', () => {
    expect(currentUserEmailVerified({ id: 1 })).toBe(true)
    expect(currentUserEmailVerified(undefined)).toBe(true)
    expect(currentUserEmailVerified(null)).toBe(true)
  })
})
