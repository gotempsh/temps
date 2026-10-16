// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ReadFailure } from '@/components/ui/read-failure'
import {
  collectRouteQueryFailures,
  representativeFailure,
  subscribeToHandledReadFailures,
  type RouteQueryFailure,
} from '@/lib/route-query-failures'
import { useIsFetching, useQueryClient } from '@tanstack/react-query'
import { useCallback, useRef, useSyncExternalStore } from 'react'

function sameFailures(a: RouteQueryFailure[], b: RouteQueryFailure[]): boolean {
  return (
    a.length === b.length &&
    a.every((f, i) => f.queryHash === b[i].queryHash && f.error === b[i].error)
  )
}

/**
 * Tells the user that part of the page they are looking at could not be
 * loaded -- the answer pages that render `data ?? []` never give.
 *
 * Mount it once, above the routes. It renders nothing while everything
 * loaded, so a healthy page pays for one cache subscription.
 */
export function RouteQueryFailureBanner() {
  const queryClient = useQueryClient()
  const snapshot = useRef<RouteQueryFailure[]>([])

  const subscribe = useCallback(
    (notify: () => void) => {
      const stopCache = queryClient.getQueryCache().subscribe(notify)
      const stopHandled = subscribeToHandledReadFailures(notify)
      return () => {
        stopCache()
        stopHandled()
      }
    },
    [queryClient]
  )
  // `useSyncExternalStore` needs a referentially stable snapshot: hand back the
  // previous array until the set of failures actually changes.
  const getSnapshot = useCallback(() => {
    const next = collectRouteQueryFailures(queryClient)
    if (!sameFailures(snapshot.current, next)) snapshot.current = next
    return snapshot.current
  }, [queryClient])
  const failures = useSyncExternalStore(subscribe, getSnapshot, getSnapshot)

  const retrying =
    useIsFetching({ predicate: (q) => q.state.status === 'error' }) > 0

  const retry = useCallback(() => {
    const hashes = new Set(failures.map((f) => f.queryHash))
    void queryClient.refetchQueries({
      predicate: (query) => hashes.has(query.queryHash),
    })
  }, [failures, queryClient])

  const failure = representativeFailure(failures)
  if (!failure) return null

  return (
    <div className="px-2 pb-4 sm:px-0" data-route-query-failure>
      <ReadFailure
        resource={
          failures.length > 1 ? 'Some data on this page' : 'Data on this page'
        }
        error={failure.error}
        onRetry={retry}
        retrying={retrying}
        routeLevel
      />
    </div>
  )
}
