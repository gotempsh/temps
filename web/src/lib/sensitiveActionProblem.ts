import type { ProblemDetails } from '@/api/client'

export type SensitiveActionProblem = ProblemDetails & {
  error_code?: string
  mfa_setup_required?: boolean
}

function extension<T>(
  problem: SensitiveActionProblem,
  key: string
): T | undefined {
  const direct = problem[key as keyof SensitiveActionProblem]
  if (direct !== undefined) return direct as T
  return problem.extensions?.[key] as T | undefined
}

export function isStepUpRequired(
  error: unknown
): error is SensitiveActionProblem {
  if (!error || typeof error !== 'object') return false
  const problem = error as SensitiveActionProblem
  return extension<string>(problem, 'error_code') === 'STEP_UP_REQUIRED'
}

export function requiresMfaSetup(problem: SensitiveActionProblem): boolean {
  return extension<boolean>(problem, 'mfa_setup_required') === true
}
