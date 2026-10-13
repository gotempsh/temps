// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Wording for the result of `POST /notification-providers/{id}/test`.
 *
 * The server's `message` states what was actually confirmed (e.g. which SMTP
 * server accepted the message) or why delivery failed and what to check, so
 * it is always preferred over a generic sentence.
 */

export type ProviderTestResult =
  { success: boolean; message?: string | null } | undefined

const FAILURE_FALLBACK =
  'The provider rejected the test notification. Check its configuration.'

export function providerTestSucceeded(result: ProviderTestResult): boolean {
  return result?.success === true
}

export function providerTestSuccessMessage(result: ProviderTestResult): string {
  return result?.message || 'Test notification sent'
}

export function providerTestFailureMessage(
  result: ProviderTestResult,
  error?: unknown
): string {
  if (result?.message) return result.message
  if (error && typeof error === 'object') {
    for (const key of ['message', 'detail'] as const) {
      const value = (error as Record<string, unknown>)[key]
      if (typeof value === 'string' && value) return value
    }
  }
  return FAILURE_FALLBACK
}
