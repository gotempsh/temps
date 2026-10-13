// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { extractProblemDetails } from '@/utils/errorHandling'

/**
 * Why a read failed, in the three ways a console page must tell apart.
 *
 * - `not-found`: the server answered 404. Only this proves a record is gone.
 * - `forbidden`: the server refused the caller (401/403). The record may well
 *   exist; the user lacks a permission, which is a different fix.
 * - `failed`: anything else -- a 5xx, a network error, an unparsable body.
 *   Nothing is known about the record.
 */
export type ReadFailureKind = 'not-found' | 'forbidden' | 'failed'

/** Only a numeric HTTP 404 establishes that a record is missing. */
export function isVerifiedNotFound(error: unknown): boolean {
  return extractProblemDetails(error)?.status === 404
}

export function isForbiddenRead(error: unknown): boolean {
  const problem = extractProblemDetails(error)
  return (
    problem?.status === 401 ||
    problem?.status === 403 ||
    problem?.title === 'Forbidden' ||
    problem?.title === 'Unauthorized'
  )
}

export function readFailureKind(error: unknown): ReadFailureKind {
  if (isVerifiedNotFound(error)) return 'not-found'
  if (isForbiddenRead(error)) return 'forbidden'
  return 'failed'
}

export function readFailureExplanation(error: unknown): string {
  if (isForbiddenRead(error)) {
    return 'You do not have permission to read this resource. Sign in with an authorized account or ask an administrator for access.'
  }
  return 'Could not contact Temps or the server could not complete the request. Retry to check its current state.'
}

/**
 * The server's own reason for a failed read, when it sent one.
 *
 * Only an RFC 7807 `detail` counts. A thrown JavaScript error's `message`
 * (`Failed to fetch`, react-query's `... data is undefined`) describes the
 * console's internals, not the request, and is never shown as the reason.
 */
export function readFailureServerDetail(error: unknown): string | undefined {
  // `extractProblemDetails` checks a body's shape, not its field types, so a
  // malformed response can carry a non-string `detail`. Rendering must not
  // throw on it: this runs inside the very component meant to recover.
  const detail: unknown = extractProblemDetails(error)?.detail
  return typeof detail === 'string' ? detail.trim() || undefined : undefined
}
