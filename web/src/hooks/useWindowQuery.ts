// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  useQuery,
  type DefaultError,
  type QueryKey,
  type UseQueryOptions,
  type UseQueryResult,
} from '@tanstack/react-query'
import { keepPreviousWindow } from '@/lib/refresh'

/**
 * `useQuery` for a query over a relative window that Refresh moves: the
 * current data stays on screen while the moved window loads, and only then
 * (see `keepPreviousWindow`).
 */
export function useWindowQuery<
  TQueryFnData = unknown,
  TError = DefaultError,
  TData = TQueryFnData,
  TQueryKey extends QueryKey = QueryKey,
>(
  options: UseQueryOptions<TQueryFnData, TError, TData, TQueryKey>
): UseQueryResult<TData, TError> {
  return useQuery({
    ...options,
    placeholderData: keepPreviousWindow(options.queryKey),
  })
}
