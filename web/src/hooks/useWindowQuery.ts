// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  hashKey,
  useQuery,
  useQueryClient,
  type DefaultError,
  type QueryKey,
  type UseQueryOptions,
  type UseQueryResult,
} from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import {
  forgetWindowMove,
  isMovingWindows,
  keepPreviousWindow,
  previousWindowQuery,
  recordWindowMove,
} from '@/lib/refresh'
import { markReadFailureRetained } from '@/lib/route-query-failures'

/**
 * Tell window moves from new selections for queries whose keys hold a
 * window, one key per query in a stable order: a key that changes while
 * `moveWindows` runs (Refresh, a relative range advancing) is a moved window
 * and keeps the data it showed (`keepPreviousWindow`); any other change is a
 * new selection and loads from scratch.
 */
export function useWindowMoves(queryKeys: readonly QueryKey[]): void {
  const queryCache = useQueryClient().getQueryCache()
  const hashes = queryKeys.map((key) => hashKey(key))
  const signature = hashes.join('\n')
  const [shown, setShown] = useState({ signature, queryKeys })
  if (shown.signature !== signature) {
    const moving = isMovingWindows()
    queryKeys.forEach((key, i) => {
      const from = shown.queryKeys[i]
      if (from !== undefined && hashKey(from) === hashes[i]) return
      if (moving && from !== undefined) recordWindowMove(queryCache, from, key)
      else forgetWindowMove(key)
    })
    setShown({ signature, queryKeys })
  }
}

/**
 * `useQuery` for a query over a relative window that Refresh moves: the
 * current data stays on screen while the moved window loads, and only then
 * (see `useWindowMoves`).
 *
 * If the moved window fails before it loads, the last window that did load
 * stays on screen rather than the page's error state, and the route's
 * failure banner says the latest data could not be loaded, with Retry.
 */
export function useWindowQuery<
  TQueryFnData = unknown,
  TError = DefaultError,
  TData = TQueryFnData,
  TQueryKey extends QueryKey = QueryKey,
>(
  options: UseQueryOptions<TQueryFnData, TError, TData, TQueryKey>
): UseQueryResult<TData, TError> {
  const queryClient = useQueryClient()
  useWindowMoves([options.queryKey])
  const result = useQuery({
    ...options,
    placeholderData: keepPreviousWindow(options.queryKey),
  })
  const previous =
    result.isError && result.data === undefined
      ? previousWindowQuery(queryClient.getQueryCache(), options.queryKey)
      : undefined
  const keptError = previous ? result.error : null
  // The page shows an earlier window: the shell says the latest did not load.
  useEffect(() => {
    if (keptError) markReadFailureRetained(keptError)
  }, [keptError])
  if (!previous) return result
  const data = previous.state.data as TQueryFnData
  return {
    ...result,
    data: options.select ? options.select(data) : data,
    error: null,
    isError: false,
    isLoadingError: false,
    isRefetchError: false,
    isSuccess: true,
    status: 'success',
    isPlaceholderData: true,
  } as UseQueryResult<TData, TError>
}
