// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient } from '@tanstack/react-query'
import { httpStatusOf } from './http-error-status'
import { isForbiddenRead } from './read-failure'

/**
 * A page cannot be trusted to look at `isError`. Many routes read their data
 * through `useQuery` and render `data ?? []` or a default, so a 500, a 403 or a
 * dropped connection leaves a normal-looking, empty page and the user is told
 * nothing. This module is the safety net under those pages: it finds the reads
 * that failed while something on screen is waiting for them, so the shell can
 * report them once, in one place, whether or not the page reports them itself.
 */

/**
 * Statuses worth putting in front of the user for a read nobody handled.
 *
 * A 404 (and the rest of the 4xx range) is an answer to a specific question --
 * "that record does not exist" -- which the page asking it is expected to turn
 * into its own not-found state; many reads 404 on purpose to mean "not set up
 * yet". 401 is the session ending, which has its own flow. What is left is
 * what the user cannot work around: the server failed, the network dropped, or
 * the account may not read this.
 */
export function isReportableReadFailure(error: unknown): boolean {
  const status = httpStatusOf(error)
  if (status === undefined) return true
  if (isForbiddenRead(error)) return status === 403
  return status >= 500 || status === 408 || status === 429
}

/** Errors a rendered component already explains to the user. */
const handledErrors = new WeakSet<object>()
const handledListeners = new Set<() => void>()

/**
 * Record that `error` is being shown by the component that asked for the data,
 * so the route-level report does not repeat it next to the page's own message.
 */
export function markReadFailureHandled(error: unknown): void {
  if (error === null || typeof error !== 'object') return
  if (handledErrors.has(error)) return
  handledErrors.add(error)
  handledListeners.forEach((listener) => listener())
}

/** Be told when a component starts explaining an error itself. */
export function subscribeToHandledReadFailures(
  listener: () => void
): () => void {
  handledListeners.add(listener)
  return () => {
    handledListeners.delete(listener)
  }
}

function isHandled(error: unknown): boolean {
  return error !== null && typeof error === 'object' && handledErrors.has(error)
}

export interface RouteQueryFailure {
  queryHash: string
  error: unknown
}

/**
 * Reads that something on screen is waiting on, that failed, have nothing to show, and
 * that no component has explained.
 *
 * A failed *refetch* over data already on screen is left alone: the page still
 * shows the last good answer, and a poll that fails once in a while must not
 * make a banner flash.
 */
export function collectRouteQueryFailures(
  client: QueryClient
): RouteQueryFailure[] {
  return client
    .getQueryCache()
    .findAll({
      predicate: (query) =>
        query.state.status === 'error' &&
        query.state.data === undefined &&
        !query.isDisabled() &&
        query.getObserversCount() > 0 &&
        isReportableReadFailure(query.state.error) &&
        !isHandled(query.state.error),
    })
    .map((query) => ({ queryHash: query.queryHash, error: query.state.error }))
}

/**
 * The one error that best explains a set of failures. A single non-permission
 * failure outranks any number of refusals: it is the one a retry can fix.
 */
export function representativeFailure(
  failures: RouteQueryFailure[]
): RouteQueryFailure | undefined {
  return failures.find((f) => !isForbiddenRead(f.error)) ?? failures[0]
}
