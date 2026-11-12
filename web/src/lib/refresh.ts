// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { hashKey, type Query, type QueryKey } from '@tanstack/react-query'

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
const WINDOW_START_PARAMS = new Set(['start_date', 'start_time', 'from'])
const WINDOW_END_PARAMS = new Set(['end_date', 'end_time', 'to'])
const WINDOW_PARAMS = new Set([...WINDOW_START_PARAMS, ...WINDOW_END_PARAMS])

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

function instant(value: unknown): number | undefined {
  if (typeof value === 'number') return value
  if (typeof value !== 'string' || value === '') return undefined
  const parsed = Date.parse(value)
  return Number.isNaN(parsed) ? undefined : parsed
}

/** The first start and end a key names, wherever in the key they sit. */
function windowOf(value: unknown): { start?: number; end?: number } {
  const found: { start?: number; end?: number } = {}
  const visit = (node: unknown) => {
    if (Array.isArray(node)) return node.forEach(visit)
    if (node === null || typeof node !== 'object') return
    for (const [key, entry] of Object.entries(node)) {
      if (WINDOW_START_PARAMS.has(key)) found.start ??= instant(entry)
      else if (WINDOW_END_PARAMS.has(key)) found.end ??= instant(entry)
      else visit(entry)
    }
  }
  visit(value)
  return found
}

/**
 * Whether `next` is `previous` moved forward the way Refresh moves a relative
 * window: the end later, the start no earlier and moved no more than the end
 * ("last 24 hours" moves both, "today" only its end), and either still
 * overlapping it or ending about `now`, as a window Refresh moved does even
 * after an idle hour on "last hour". Picking another range (7 days after 24
 * hours, 24 hours after 7 days, or a custom range elsewhere in the past) is
 * not. Bounds a key does not name are not judged.
 */
function movedForward(
  previous: { start?: number; end?: number },
  next: { start?: number; end?: number },
  now: number
): boolean {
  const { start: s0, end: e0 } = previous
  const { start: s1, end: e1 } = next
  if (e0 !== undefined && e1 !== undefined && e1 < e0) return false
  if (s0 !== undefined && s1 !== undefined && s1 < s0) return false
  if (s0 === undefined || s1 === undefined) return true
  if (e0 === undefined || e1 === undefined) return true
  if (s1 - s0 > e1 - e0) return false
  return s1 < e0 || Math.abs(now - e1) <= e1 - s1
}

/**
 * `placeholderData` for a query whose window moves: while the moved window
 * loads, the previous window's data stays on screen. Any other change to the
 * key (a drill-down, another service, filter or page, or another range picked)
 * gets no placeholder, so data of one selection never shows, clickable, under
 * another.
 */
export function keepPreviousWindow(queryKey: QueryKey, now = Date.now()) {
  const current = hashKey(withoutWindow(queryKey) as QueryKey)
  const currentWindow = windowOf(queryKey)
  return <T>(
    previous: T | undefined,
    previousQuery?: { queryKey: QueryKey }
  ): T | undefined =>
    previousQuery !== undefined &&
    hashKey(withoutWindow(previousQuery.queryKey) as QueryKey) === current &&
    movedForward(windowOf(previousQuery.queryKey), currentWindow, now)
      ? previous
      : undefined
}

/**
 * The query for the last window that loaded before `queryKey`'s window: the
 * same selection, a window `queryKey` moved forward from (as of `now`, when
 * it was asked for), with data. When a moved window fails before it loads,
 * the page keeps showing this one.
 */
export function previousWindowQuery(
  queries: readonly Query[],
  queryKey: QueryKey,
  now: number
): Query | undefined {
  const current = hashKey(withoutWindow(queryKey) as QueryKey)
  const currentWindow = windowOf(queryKey)
  let latest: Query | undefined
  for (const query of queries) {
    if (query.state.data === undefined) continue
    if (hashKey(withoutWindow(query.queryKey) as QueryKey) !== current) continue
    if (!movedForward(windowOf(query.queryKey), currentWindow, now)) continue
    if (!latest || query.state.dataUpdatedAt > latest.state.dataUpdatedAt)
      latest = query
  }
  return latest
}
