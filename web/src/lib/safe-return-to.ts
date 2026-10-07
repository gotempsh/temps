// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `?returnTo=` support for prerequisite setup flows ("add a provider first",
 * "create a source first"): the setup page sends the user back to the task
 * they came from instead of to its own list page.
 *
 * Only same-origin, in-app paths are accepted. Anything else (absolute URLs,
 * protocol-relative `//host`, backslash tricks, control characters) is treated
 * as absent, so a crafted link cannot turn a setup page into an open redirect.
 */

const PLACEHOLDER_ORIGIN = 'https://temps.invalid'
const PARAM = 'returnTo'

export function safeReturnTo(value: string | null | undefined): string | null {
  if (!value || !value.startsWith('/') || value.startsWith('//')) return null
  // Browsers normalise `\` to `/`, so `/\host` would become `//host`.
  // eslint-disable-next-line no-control-regex
  if (/[\\\s\u0000-\u001f\u007f]/.test(value)) return null
  let url: URL
  try {
    url = new URL(value, PLACEHOLDER_ORIGIN)
  } catch {
    return null
  }
  if (url.origin !== PLACEHOLDER_ORIGIN) return null
  return `${url.pathname}${url.search}${url.hash}`
}

/** Read and validate the `returnTo` parameter from a page's search params. */
export function returnToFromSearch(params: URLSearchParams): string | null {
  return safeReturnTo(params.get(PARAM))
}

/**
 * `path` with `returnTo` appended, preserving any query string and hash
 * already on `path`. An unsafe `returnTo` is dropped rather than forwarded.
 */
export function withReturnTo(
  path: string,
  returnTo: string | null | undefined
): string {
  const safe = safeReturnTo(returnTo)
  if (!safe) return path
  const url = new URL(path, PLACEHOLDER_ORIGIN)
  url.searchParams.set(PARAM, safe)
  return `${url.pathname}${url.search}${url.hash}`
}

/** The in-app path of a router location, for use as a `returnTo` value. */
export function locationPath(location: {
  pathname: string
  search: string
  hash?: string
}): string {
  return `${location.pathname}${location.search}${location.hash ?? ''}`
}

/**
 * Router state attached when a setup page sends the user back to `returnTo`.
 * The task page then knows its history now contains the setup detour, so a
 * history-based "go back" would land on the setup page instead of where the
 * task started; it should navigate to its own list page instead.
 */
export const RETURNED_FROM_SETUP_STATE = { returnedFromSetup: true } as const

export function isReturnFromSetup(state: unknown): boolean {
  return (
    typeof state === 'object' &&
    state !== null &&
    (state as { returnedFromSetup?: unknown }).returnedFromSetup === true
  )
}

/** Navigation options for going back to a validated `returnTo`. */
export function returnNavigation() {
  return { replace: true, state: RETURNED_FROM_SETUP_STATE }
}
