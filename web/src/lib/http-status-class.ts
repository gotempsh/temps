// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Classifies an HTTP status code into its RFC 9110 class.
 *
 * 1xx (informational, e.g. 101 Switching Protocols for a WebSocket/SSE
 * upgrade) is a distinct, non-error class -- it must never be treated as an
 * error or fall through to an "unknown" bucket alongside genuinely
 * unrecognized codes.
 */
export type HttpStatusClass = '1xx' | '2xx' | '3xx' | '4xx' | '5xx' | 'unknown'

export function httpStatusClass(statusCode: number): HttpStatusClass {
  if (statusCode >= 100 && statusCode < 200) return '1xx'
  if (statusCode >= 200 && statusCode < 300) return '2xx'
  if (statusCode >= 300 && statusCode < 400) return '3xx'
  if (statusCode >= 400 && statusCode < 500) return '4xx'
  if (statusCode >= 500 && statusCode < 600) return '5xx'
  return 'unknown'
}

export function isHttpErrorStatus(statusCode: number): boolean {
  const cls = httpStatusClass(statusCode)
  return cls === '4xx' || cls === '5xx'
}
