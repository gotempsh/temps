// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProblemDetails } from '@/api/client'

type MfaChallengeProblem = ProblemDetails & {
  error_code?: string
}

/**
 * Whether a failed `POST /auth/verify-mfa` means the sign-in attempt itself
 * is over (missing, expired, or discarded after too many wrong codes), so
 * another code can never succeed and the user has to sign in again.
 */
export function isMfaChallengeEnded(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false
  const problem = error as MfaChallengeProblem
  const code =
    typeof problem.error_code === 'string'
      ? problem.error_code
      : problem.extensions?.error_code
  return code === 'MFA_CHALLENGE_ENDED'
}
