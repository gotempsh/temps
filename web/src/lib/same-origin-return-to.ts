// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `?returnTo=` support for pages that send the operator somewhere to finish a
 * prerequisite (create a backup destination, a notification provider, ...)
 * and then bring them back to where they started.
 *
 * The value comes from the URL, so anyone can craft it. It is only followed
 * when it is a plain same-origin path: never another origin, a
 * protocol-relative `//host`, a backslash path some browsers treat as one, or
 * anything with whitespace or control characters smuggled in.
 */

const PARAM = 'returnTo'
const PROBE_ORIGIN = 'https://temps.invalid'

/** The same-origin path in `value`, or `null` when it must not be followed. */
export function sameOriginReturnTo(
  value: string | null | undefined
): string | null {
  if (!value) return null
  if (!value.startsWith('/') || value.startsWith('//')) return null
  if (/[^\x21-\x7e]|\\/.test(value)) return null
  let url: URL
  try {
    url = new URL(value, PROBE_ORIGIN)
  } catch {
    return null
  }
  if (url.origin !== PROBE_ORIGIN) return null
  return `${url.pathname}${url.search}${url.hash}`
}

/** Read and validate the `returnTo` parameter from a page's search params. */
export function returnToFromSearch(params: URLSearchParams): string | null {
  return sameOriginReturnTo(params.get(PARAM))
}

/**
 * `path` with `returnTo` set to `returnTo`, preserving any query string and
 * hash already on `path`. An unsafe `returnTo` is dropped rather than passed
 * along.
 */
export function withReturnTo(path: string, returnTo: string): string {
  const safe = sameOriginReturnTo(returnTo)
  if (!safe) return path
  const url = new URL(path, PROBE_ORIGIN)
  url.searchParams.set(PARAM, safe)
  return `${url.pathname}${url.search}${url.hash}`
}
