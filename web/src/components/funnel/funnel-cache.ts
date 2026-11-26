// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient } from '@tanstack/react-query'
import type { FunnelResponse } from '@/api/client/types.gen'
import {
  getFunnelMetricsQueryKey,
  getFunnelQueryKey,
  listFunnelsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'

/**
 * Brings the cached funnel queries up to date after `saved` was created or
 * updated. The generated query keys start with an object
 * (`[{ _id: 'getFunnel', baseUrl, path }]`), so they are built with the
 * generated key functions: a bare `['getFunnel']` matches nothing.
 *
 * The saved funnel is the mutation's own response, so it replaces the cached
 * read directly. Every page of the project's funnel list is refetched, and
 * the funnel's metrics too, since its steps may have changed.
 */
export function refreshSavedFunnel(
  queryClient: QueryClient,
  projectId: number,
  saved: FunnelResponse
) {
  const path = { project_id: projectId, funnel_id: saved.id }
  queryClient.setQueryData(getFunnelQueryKey({ path }), saved)
  // A key without `query` matches the list at every page and page size.
  void queryClient.invalidateQueries({
    queryKey: listFunnelsQueryKey({ path: { project_id: projectId } }),
  })
  void queryClient.invalidateQueries({
    queryKey: getFunnelMetricsQueryKey({ path }),
  })
}

/**
 * Brings the cached funnel queries up to date after funnel `funnelId` was
 * deleted: every page of the project's funnel list is refetched, and the
 * funnel's own read and metrics are dropped so nothing shows it again.
 */
export function forgetDeletedFunnel(
  queryClient: QueryClient,
  projectId: number,
  funnelId: number
) {
  const path = { project_id: projectId, funnel_id: funnelId }
  void queryClient.invalidateQueries({
    queryKey: listFunnelsQueryKey({ path: { project_id: projectId } }),
  })
  queryClient.removeQueries({ queryKey: getFunnelQueryKey({ path }) })
  queryClient.removeQueries({ queryKey: getFunnelMetricsQueryKey({ path }) })
}
