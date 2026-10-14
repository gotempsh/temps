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
 * - Object bodies are annotated in place.
 * - A body that is empty or not JSON (a reverse proxy's HTML 404, a WAF's
 *   "Access denied", a bare `Not Found`) cannot carry a status, so a 4xx one is
 *   replaced by a minimal problem document -- see `problemFromNonJsonBody`. A
 *   proxy that answered 403 is still an answer: the console must show it at
 *   once, not retry it three times and report "could not contact Temps". A
 *   5xx keeps its raw body: a string (or empty value) there still means
 *   "retry, the server may be restarting", and callers that read the string
 *   keep working.
 * - A body that already carries a `status` member keeps it: it is the
 *   server's own statement and must not be overwritten.
 */
export function attachHttpStatus<TError>(
  error: TError,
  response: Pick<Response, 'ok' | 'status'> | undefined
): TError {
  if (!response || response.ok) return error
  if (isNonJsonBody(error)) {
    if (response.status < 400 || response.status >= 500) return error
    return problemFromNonJsonBody(error, response.status) as TError
  }
  if (!error || typeof error !== 'object' || Array.isArray(error)) return error
  if ('status' in error) return error
  try {
    ;(error as { status?: number }).status = response.status
  } catch {
    // A frozen or exotic object: leave it as thrown rather than fail the call.
  }
  return error
}

/** What the generated client throws when the body is empty or not JSON. */
function isNonJsonBody(error: unknown): error is string | null | undefined {
  return error === '' || error == null || typeof error === 'string'
}

/**
 * Standard reason phrases for the 4xx statuses a proxy or WAF sends. The
 * response's own `statusText` is not used: HTTP/2 has none, so a title taken
 * from it would differ between HTTP versions, and the console keys off titles
 * (`Unauthorized`, `Forbidden`) in places.
 */
const CLIENT_ERROR_TITLES: Record<number, string> = {
  400: 'Bad Request',
  401: 'Unauthorized',
  403: 'Forbidden',
  404: 'Not Found',
  405: 'Method Not Allowed',
  408: 'Request Timeout',
  409: 'Conflict',
  410: 'Gone',
  413: 'Payload Too Large',
  414: 'URI Too Long',
  415: 'Unsupported Media Type',
  422: 'Unprocessable Entity',
  429: 'Too Many Requests',
}

/** A plain-text reason longer than this is a page, not a message. */
const MAX_PLAIN_TEXT_DETAIL = 300

/**
 * A minimal RFC 7807 document for a 4xx answer without a JSON body.
 *
 * `title` is always a string because `extractProblemDetails` ignores objects
 * without one, which would leave the status unreadable. `detail` is only set
 * for short plain text -- never for markup, so a proxy's HTML page is not
 * presented as "the server's reason".
 */
export function problemFromNonJsonBody(
  body: string | null | undefined,
  status: number
): { status: number; title: string; detail?: string } {
  const text = typeof body === 'string' ? body.trim() : ''
  const isPlainText =
    text.length > 0 &&
    text.length <= MAX_PLAIN_TEXT_DETAIL &&
    !text.includes('<')
  return {
    status,
    title: CLIENT_ERROR_TITLES[status] ?? `HTTP ${status}`,
    ...(isPlainText ? { detail: text } : {}),
  }
}
