// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { signedOutMessage } from './SignOutOfAppsCard'

describe('signedOutMessage', () => {
  test('says how many app sessions ended', () => {
    expect(signedOutMessage(0)).toBe('You were not signed in to any app.')
    expect(signedOutMessage(1)).toBe('Signed out of 1 app session.')
    expect(signedOutMessage(3)).toBe('Signed out of 3 app sessions.')
  })
})
