// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProblemDetails } from '@/api/client'

type CliDeviceProblem = ProblemDetails & {
  error_code?: string
}

function errorCode(error: unknown): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  const problem = error as CliDeviceProblem
  if (typeof problem.error_code === 'string') return problem.error_code
  const nested = problem.extensions?.error_code
  return typeof nested === 'string' ? nested : undefined
}

export type CliDeviceLookupErrorKind = 'not_found' | 'expired' | null

export function cliDeviceLookupErrorKind(
  error: unknown
): CliDeviceLookupErrorKind {
  switch (errorCode(error)) {
    case 'CLI_DEVICE_SESSION_NOT_FOUND':
      return 'not_found'
    case 'CLI_DEVICE_SESSION_EXPIRED':
      return 'expired'
    default:
      return null
  }
}
