// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { extractProblemDetails } from '@/utils/errorHandling'

/** Only a numeric HTTP 404 establishes that a record is missing. */
export function isVerifiedNotFound(error: unknown): boolean {
  return extractProblemDetails(error)?.status === 404
}

export function readFailureExplanation(error: unknown): string {
  const problem = extractProblemDetails(error)
  if (
    problem?.status === 401 ||
    problem?.status === 403 ||
    problem?.title === 'Forbidden' ||
    problem?.title === 'Unauthorized'
  ) {
    return 'You do not have permission to read this resource. Sign in with an authorized account or ask an administrator for access.'
  }
  return 'Could not contact Temps or the server could not complete the request. Retry to check its current state.'
}
