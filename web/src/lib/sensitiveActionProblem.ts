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

/** Wrap a destructive retry so duplicate verification callbacks cannot run it twice. */
export function oneShotSensitiveActionRetry(retry: () => void): () => void {
  let consumed = false
  return () => {
    if (consumed) return
    consumed = true
    retry()
  }
}

export function canCloseSensitiveActionDialog(isVerifying: boolean): boolean {
  return !isVerifying
}

export function sensitiveActionErrorMessage(
  error: unknown,
  fallback: string
): string {
  if (!error || typeof error !== 'object') return fallback
  const problem = error as { detail?: unknown; message?: unknown }
  if (typeof problem.detail === 'string' && problem.detail.trim()) {
    return problem.detail
  }
  if (typeof problem.message === 'string' && problem.message.trim()) {
    return problem.message
  }
  return fallback
}
