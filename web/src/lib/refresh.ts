// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { hashKey, type QueryKey } from '@tanstack/react-query'

type KeyScope<Options> = {
  [Key in keyof Options]?: Options[Key] extends object
    ? Partial<Options[Key]>
    : Options[Key]
}

/**
 * A prefix key for one generated endpoint, for `RefreshButton`'s `queryKeys`:
 * every cached query of `getFooQueryKey`, or only those whose `path`/`query`
 * contain `scope` (`{ path: { project_id } }`). Works for endpoints whose key
 * needs options, without inventing values for the parameters left out.
 */
export function queryKeyPrefix<Options>(
  queryKey: (options: Options) => QueryKey,
  scope: KeyScope<Options> = {}
): QueryKey {
  // The generated key copies only the parts it is given, so a partial scope
  // yields `[{ _id, baseUrl, ...scope }]`: a prefix of every full key.
  return queryKey(scope as Options)
}

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** "Updated just now", then "Updated 2m ago", "Updated 3h ago", "Updated 1d ago". */
export function formatUpdatedAgo(updatedAt: number, now: number): string {
  const elapsed = Math.max(0, now - updatedAt)
  if (elapsed < MINUTE) return 'Updated just now'
  if (elapsed < HOUR) return `Updated ${Math.floor(elapsed / MINUTE)}m ago`
  if (elapsed < DAY) return `Updated ${Math.floor(elapsed / HOUR)}h ago`
  return `Updated ${Math.floor(elapsed / DAY)}d ago`
}

/**
 * Query parameters that only place a query in time. Refresh on a relative
 * window ("last 24 hours") changes these and nothing else.
 */
const WINDOW_PARAMS = new Set([
  'start_date',
  'end_date',
  'start_time',
  'end_time',
  'from',
  'to',
])

function withoutWindow(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(withoutWindow)
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value)
        .filter(([key]) => !WINDOW_PARAMS.has(key))
        .map(([key, entry]) => [key, withoutWindow(entry)])
    )
  }
  return value
}

/**
 * `placeholderData` for a query whose window moves: while the moved window
 * loads, the previous window's data stays on screen. Any other change to the
 * key (a drill-down, another service, filter or page) gets no placeholder, so
 * rows of one selection never show, clickable, under another.
 */
export function keepPreviousWindow(queryKey: QueryKey) {
  const current = hashKey(withoutWindow(queryKey) as QueryKey)
  return <T>(
    previous: T | undefined,
    previousQuery?: { queryKey: QueryKey }
  ): T | undefined =>
    previousQuery !== undefined &&
    hashKey(withoutWindow(previousQuery.queryKey) as QueryKey) === current
      ? previous
      : undefined
}
