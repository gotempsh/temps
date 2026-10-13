// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { httpStatusOf } from './http-error-status'

/** How many times a failed read is retried when a retry can help. */
export const MAX_QUERY_RETRIES = 3

/**
 * 4xx statuses that describe a passing condition rather than the request
 * itself: a timed-out request or a rate limit can succeed on a later try.
 */
const TRANSIENT_CLIENT_STATUSES = new Set([408, 429])

/**
 * Whether a failed read is worth asking again.
 *
 * A 4xx answer is deterministic -- a missing record stays missing and a
 * refused permission stays refused -- so retrying it only delays the message
 * the user needs by several seconds and sends requests that cannot succeed.
 * Network failures and 5xx responses (a restart, a reverse proxy hiccup) are
 * often transient, so they keep react-query's usual retries.
 */
export function shouldRetryQuery(
  failureCount: number,
  error: unknown
): boolean {
  const status = httpStatusOf(error)
  if (
    status !== undefined &&
    status >= 400 &&
    status < 500 &&
    !TRANSIENT_CLIENT_STATUSES.has(status)
  ) {
    return false
  }
  return failureCount < MAX_QUERY_RETRIES
}
