// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  providerTestFailureMessage,
  providerTestSucceeded,
  providerTestSuccessMessage,
} from './notification-provider-test'

describe('provider test result wording', () => {
  it('treats a 200 response with success: false as a failure', () => {
    const result = {
      success: false,
      message:
        'Test notification was not delivered: SMTP server smtp.test:1025 (TLS mode: none) did not accept the message',
    }
    expect(providerTestSucceeded(result)).toBe(false)
    expect(providerTestFailureMessage(result)).toContain('smtp.test:1025')
  })

  it('shows what the server confirmed on success', () => {
    const result = {
      success: true,
      message: 'SMTP server smtp.test:1025 accepted the test message',
    }
    expect(providerTestSucceeded(result)).toBe(true)
    expect(providerTestSuccessMessage(result)).toBe(result.message)
    expect(providerTestSuccessMessage({ success: true })).toBe(
      'Test notification sent'
    )
  })

  it('falls back to the error body, then to a generic sentence', () => {
    expect(
      providerTestFailureMessage(undefined, {
        success: false,
        message: 'Notification provider with ID 9 not found',
      })
    ).toBe('Notification provider with ID 9 not found')
    expect(providerTestFailureMessage(undefined, { detail: 'boom' })).toBe(
      'boom'
    )
    expect(providerTestFailureMessage(undefined, null)).toContain(
      'Check its configuration'
    )
  })
})
