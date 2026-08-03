import { describe, expect, test } from 'bun:test'
import {
  isStepUpRequired,
  requiresMfaSetup,
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
})
