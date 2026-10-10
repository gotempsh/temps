// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The HTTP status of a failed API call, carried on the error the console sees.
 *
 * The server sends RFC 7807 bodies without a `status` member -- the status
 * lives only on the HTTP status line -- and the generated client throws the
 * parsed body, so without help every thrown error looks the same: a 404 for a
 * missing project, a 403 for a missing permission and a 500 during a restart
 * are indistinguishable. `attachHttpStatus` copies the status line onto the
 * thrown body, which is what lets the console tell "not found" from "could
 * not load" and decide what is worth retrying.
 */

/** A status code the console can rely on, or undefined when none is known. */
export function httpStatusOf(error: unknown): number | undefined {
  if (!error || typeof error !== 'object') return undefined
  const status = (error as { status?: unknown }).status
  return typeof status === 'number' && Number.isInteger(status)
    ? status
    : undefined
}

/**
 * Error interceptor for the generated API client: record the response's HTTP
 * status on the thrown error.
 *
 * - Only a failed response (`!response.ok`) has a status worth recording. A
 *   network failure has no response, and an error thrown while parsing a 2xx
 *   body is not an HTTP failure; both pass through untouched.
 * - Only object bodies are annotated. A non-JSON body (a reverse proxy's HTML
 *   502 page) is thrown as a string, and several callers read that string
 *   directly, so it is not rewrapped.
 * - A body that already carries a `status` member keeps it: it is the
 *   server's own statement and must not be overwritten.
 */
export function attachHttpStatus<TError>(
  error: TError,
  response: Pick<Response, 'ok' | 'status'> | undefined
): TError {
  if (!response || response.ok) return error
  if (!error || typeof error !== 'object' || Array.isArray(error)) return error
  if ('status' in error) return error
  try {
    ;(error as { status?: number }).status = response.status
  } catch {
    // A frozen or exotic object: leave it as thrown rather than fail the call.
  }
  return error
}
