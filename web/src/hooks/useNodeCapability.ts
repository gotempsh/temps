// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { nodeCapabilityGetOptions } from '@/api/client/@tanstack/react-query.gen'
import type { NodeCapabilityResponse } from '@/api/client/types.gen'
import { nodeCapabilityRefetchInterval } from '@/lib/worker-nodes'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback } from 'react'

/** Scheduling capability of this installation, as served by the API. */
export type NodeCapability = NodeCapabilityResponse

/**
 * Cache key for the capability read.
 *
 * Taken from the generated query options so it cannot drift from the key the
 * hook below actually writes under — code outside React (the global mutation
 * error handler in `App.tsx`) reads the cache by this key.
 */
export const nodeCapabilityQueryKey = nodeCapabilityGetOptions().queryKey

/**
 * Whether this installation can run a workload (locally or on a worker node).
 *
 * Fails *open*: any read failure leaves `data` undefined, which every consumer
 * treats as "say nothing" (see `shouldShowWorkerNodeBanner`). A server that
 * predates the endpoint therefore never tells an operator their platform is
 * broken. `retry: false` keeps that from costing four requests.
 *
 * Refreshing matters as much as reading: the fix for "nothing can run here" —
 * `temps join` on another machine — happens entirely outside this browser, and
 * window-focus refetching is disabled app-wide, so without a poll the banner
 * would outlive the problem until the page was remounted.
 */
export function useNodeCapability() {
  return useQuery({
    ...nodeCapabilityGetOptions(),
    retry: false,
    // Short enough that a `temps join` in another tab or terminal shows up on
    // the next navigation, long enough that mounting three banners on one
    // page is still one request.
    staleTime: 15_000,
    refetchInterval: (query) => nodeCapabilityRefetchInterval(query.state.data),
    // Deliberately overrides the app-wide `refetchOnWindowFocus: false`: an
    // operator who alt-tabs to a terminal, runs `temps join` and comes back
    // must not be told the platform still cannot run anything.
    refetchOnWindowFocus: true,
  })
}

/**
 * Re-read the capability after something that changes what can be scheduled.
 *
 * Used by the Worker Nodes page: joining, draining, reactivating or removing a
 * node all move the answer, and a cached "nothing can run here" is exactly the
 * state an operator has just finished fixing.
 */
export function useInvalidateNodeCapability() {
  const queryClient = useQueryClient()
  return useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: nodeCapabilityQueryKey })
  }, [queryClient])
}
