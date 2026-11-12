// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  useQuery,
  useQueryClient,
  type DefaultError,
  type QueryKey,
  type UseQueryOptions,
  type UseQueryResult,
} from '@tanstack/react-query'
import { useEffect } from 'react'
import { keepPreviousWindow, previousWindowQuery } from '@/lib/refresh'
import { markReadFailureHandled } from '@/lib/route-query-failures'

/**
 * `useQuery` for a query over a relative window that Refresh moves: the
 * current data stays on screen while the moved window loads, and only then
 * (see `keepPreviousWindow`).
 *
 * If the moved window fails before it loads, the last window that did load
 * stays on screen rather than the page's error state: Refresh reports the
 * failure ("Couldn't refresh"), and the next refresh tries again.
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
  const result = useQuery({
    ...options,
    placeholderData: keepPreviousWindow(options.queryKey),
  })
  const previous =
    result.isError && result.data === undefined
      ? previousWindowQuery(
          queryClient.getQueryCache().getAll(),
          options.queryKey,
          // Close to when the moved window was asked for, and stable
          // across renders.
          result.errorUpdatedAt
        )
      : undefined
  const keptError = previous ? result.error : null
  // The page shows the earlier window, so the shell has nothing to report.
  useEffect(() => {
    if (keptError) markReadFailureHandled(keptError)
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
