// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  hashKey,
  type Query,
  type QueryCache,
  type QueryKey,
} from '@tanstack/react-query'
import { flushSync } from 'react-dom'

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
  'start_time',
  'from',
  'end_date',
  'end_time',
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
 * Window moves in progress: Refresh, or a relative range advancing on its own.
 * Only a key change made inside `moveWindows` is a move; any other (another
 * range picked, however close to now it ends) is a new selection.
 */
let moving = 0

/**
 * Move windows: run `update`, the state change that moves one or more
 * relative windows to now, and render it synchronously, so each window query
 * sees its key change while the move is in progress (`isMovingWindows`).
 */
export function moveWindows(update: () => void): void {
  moving += 1
  try {
    flushSync(update)
  } finally {
    moving -= 1
  }
}

/** Whether the key changes rendered now are window moves. */
export function isMovingWindows(): boolean {
  return moving > 0
}

/**
 * For each moved key (its hash), the window whose data it shows until it
 * loads, and falls back to if it fails. Bounded: the oldest moves are
 * forgotten first.
 */
const movedFrom = new Map<string, string>()
const MOVES_REMEMBERED = 500

/**
 * Record that a query's key moved from `from` to `to`. The data `to` keeps on
 * screen is `from`'s, or, if `from` had not loaded either, what `from` was
 * keeping. A change to anything but the window is not a move.
 */
export function recordWindowMove(
  queryCache: QueryCache,
  from: QueryKey,
  to: QueryKey
): void {
  const toHash = hashKey(to)
  movedFrom.delete(toHash)
  if (
    hashKey(withoutWindow(from) as QueryKey) !==
    hashKey(withoutWindow(to) as QueryKey)
  )
    return
  const fromHash = hashKey(from)
  const origin =
    queryCache.get(fromHash)?.state.data !== undefined
      ? fromHash
      : movedFrom.get(fromHash)
  if (origin === undefined || origin === toHash) return
  movedFrom.set(toHash, origin)
  if (movedFrom.size > MOVES_REMEMBERED) {
    const [oldest] = movedFrom.keys()
    movedFrom.delete(oldest)
  }
}

/** Record that `to` is a new selection, not a moved window. */
export function forgetWindowMove(to: QueryKey): void {
  movedFrom.delete(hashKey(to))
}

/**
 * `placeholderData` for a query whose window moves: while the moved window
 * loads, the previous window's data stays on screen. Any other change to the
 * key (a drill-down, another service, filter or page, or another range
 * picked) gets no placeholder, so data of one selection never shows,
 * clickable, under another. Moves are recorded by `useWindowMoves`.
 */
export function keepPreviousWindow(queryKey: QueryKey) {
  const current = hashKey(queryKey)
  return <T>(
    previous: T | undefined,
    previousQuery?: { queryKey: QueryKey }
  ): T | undefined =>
    previousQuery !== undefined &&
    movedFrom.get(current) === hashKey(previousQuery.queryKey)
      ? previous
      : undefined
}

/**
 * The query whose data `queryKey`'s moved window keeps on screen, if it still
 * has data: when a moved window fails before it loads, the page keeps showing
 * that one.
 */
export function previousWindowQuery(
  queryCache: QueryCache,
  queryKey: QueryKey
): Query | undefined {
  const from = movedFrom.get(hashKey(queryKey))
  if (from === undefined) return undefined
  const query = queryCache.get(from)
  return query?.state.data === undefined ? undefined : query
}
