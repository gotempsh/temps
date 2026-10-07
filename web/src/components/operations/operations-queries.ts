// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Infinite-query options for the header operations tray. Active work and
 * finished history are separate queries against `GET /operations` so the
 * in-flight rows and the badge count always come from the same response,
 * however much newer finished work exists.
 */
import { listOperationsInfiniteOptions } from '@/api/client/@tanstack/react-query.gen'
import type { Options } from '@/api/client/sdk.gen'
import type { ListOperationsData } from '@/api/client/types.gen'
import {
  FINISHED_OPERATIONS_QUERY,
  operationsNextPage,
  RUNNING_OPERATIONS_QUERY,
} from '@/lib/operations'

/** Lets tests point the queries at a stub client. */
export type OperationsQueryOverrides = Pick<
  Options<ListOperationsData>,
  'client'
>

/** In-flight operations (`status=running`), 100 per page. */
export function runningOperationsInfiniteOptions(
  overrides: OperationsQueryOverrides = {}
) {
  return {
    ...listOperationsInfiniteOptions({
      ...overrides,
      query: { ...RUNNING_OPERATIONS_QUERY },
    }),
    initialPageParam: 1,
    getNextPageParam: operationsNextPage,
  }
}

/** Finished operations (`status=finished`), 20 per page. */
export function finishedOperationsInfiniteOptions(
  overrides: OperationsQueryOverrides = {}
) {
  return {
    ...listOperationsInfiniteOptions({
      ...overrides,
      query: { ...FINISHED_OPERATIONS_QUERY },
    }),
    initialPageParam: 1,
    getNextPageParam: operationsNextPage,
  }
}
