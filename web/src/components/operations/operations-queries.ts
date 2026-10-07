// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Query options for the header operations tray. Active work and finished
 * history are separate queries against `GET /operations` so the in-flight
 * rows and the badge count always come from the same response, however much
 * newer finished work exists.
 *
 * Both sections use page navigation, not infinite scrolling: each section
 * observes exactly one page (keyed by page number) and Newer/Older replaces
 * it. Memory bound: per section the tray holds at most three pages -- the
 * first page (cached for the badge), the page on screen, and the page being
 * left while its successor loads (`keepPreviousData`). That is at most
 * 3 x 100 running + 3 x 20 finished = 360 rows however long the user
 * browses, and 120 rows while the tray is closed. Pages other than the first
 * are dropped from the cache as soon as nothing observes them (`gcTime: 0`),
 * and closing the tray returns both sections to page 1, so nothing loaded
 * while browsing outlives the open tray.
 */
import { listOperationsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { Options } from '@/api/client/sdk.gen'
import type { ListOperationsData } from '@/api/client/types.gen'
import {
  FINISHED_OPERATIONS_QUERY,
  RUNNING_OPERATIONS_QUERY,
} from '@/lib/operations'
import { keepPreviousData } from '@tanstack/react-query'

/** Lets tests point the queries at a stub client. */
export type OperationsQueryOverrides = Pick<
  Options<ListOperationsData>,
  'client'
>

/**
 * Cache lifetime for a page the user browsed to. The first page backs the
 * badge and keeps the client default; any other page is released as soon as
 * it is no longer shown.
 */
function browsedPageCache(page: number): { gcTime?: number } {
  return page > 1 ? { gcTime: 0 } : {}
}

/** One page of in-flight operations (`status=running`), 100 per page. */
export function runningOperationsPageOptions(
  page: number,
  overrides: OperationsQueryOverrides = {}
) {
  return {
    ...listOperationsOptions({
      ...overrides,
      query: { ...RUNNING_OPERATIONS_QUERY, page },
    }),
    placeholderData: keepPreviousData,
    ...browsedPageCache(page),
  }
}

/** One page of finished operations (`status=finished`), 20 per page. */
export function finishedOperationsPageOptions(
  page: number,
  overrides: OperationsQueryOverrides = {}
) {
  return {
    ...listOperationsOptions({
      ...overrides,
      query: { ...FINISHED_OPERATIONS_QUERY, page },
    }),
    placeholderData: keepPreviousData,
    ...browsedPageCache(page),
  }
}
