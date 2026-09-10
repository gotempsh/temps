// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isStepUpRequired,
  canCloseSensitiveActionDialog,
  oneShotSensitiveActionRetry,
  requiresMfaSetup,
  sensitiveActionErrorMessage,
  type SensitiveActionProblem,
} from './sensitiveActionProblem'

function problem(
  fields: Partial<SensitiveActionProblem>
): SensitiveActionProblem {
  return {
    title: 'Additional Verification Required',
    extensions: {},
    ...fields,
  }
}

describe('sensitive action problems', () => {
  test('recognizes the flattened runtime Problem Details shape', () => {
    const value = problem({
      error_code: 'STEP_UP_REQUIRED',
      mfa_setup_required: true,
    })
    expect(isStepUpRequired(value)).toBe(true)
    expect(requiresMfaSetup(value)).toBe(true)
  })

  test('recognizes the generated nested extensions shape', () => {
    const value = problem({
      extensions: {
        error_code: 'STEP_UP_REQUIRED',
        mfa_setup_required: false,
      },
    })
    expect(isStepUpRequired(value)).toBe(true)
    expect(requiresMfaSetup(value)).toBe(false)
  })

  test('does not intercept unrelated failures', () => {
    expect(
      isStepUpRequired(problem({ extensions: { error_code: 'FORBIDDEN' } }))
    ).toBe(false)
    expect(isStepUpRequired(new Error('network failure'))).toBe(false)
  })

  test('a sensitive-action retry can only run once', () => {
    let attempts = 0
    const retry = oneShotSensitiveActionRetry(() => attempts++)

    retry()
    retry()

    expect(attempts).toBe(1)
  })

  test('the dialog cannot close while verification is in flight', () => {
    expect(canCloseSensitiveActionDialog(false)).toBe(true)
    expect(canCloseSensitiveActionDialog(true)).toBe(false)
  })

  test('unrelated mutation failures retain an actionable message', () => {
    expect(
      sensitiveActionErrorMessage(
        { detail: 'The API key name is already in use.' },
        'Could not create API key'
      )
    ).toBe('The API key name is already in use.')
    expect(
      sensitiveActionErrorMessage(new Error('Network unavailable'), 'Fallback')
    ).toBe('Network unavailable')
    expect(sensitiveActionErrorMessage({}, 'Fallback')).toBe('Fallback')
  })
})
