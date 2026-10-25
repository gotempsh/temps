// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { resetMfaSuccessMessage } from './reset-mfa-message'

test('the success message counts the sessions that were signed out', () => {
  expect(resetMfaSuccessMessage('Example User', 1)).toBe(
    'MFA removed for Example User. 1 session signed out.'
  )
  expect(resetMfaSuccessMessage('Example User', 0)).toBe(
    'MFA removed for Example User. 0 sessions signed out.'
  )
  expect(resetMfaSuccessMessage('Example User', 3)).toBe(
    'MFA removed for Example User. 3 sessions signed out.'
  )
})
