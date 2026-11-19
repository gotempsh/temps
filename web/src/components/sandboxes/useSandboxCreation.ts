// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getSandboxCreationOptions } from '@/api/client/@tanstack/react-query.gen'
import { CREATION_POLL_MS, keepPolling } from './sandbox-creation'

/**
 * Polls the steps of the create sent with `creationId` while it runs. A 404
 * before the server has started tracking it is retried while the create
 * request is still in flight (`requestSettled` false): the request and the
 * first poll race. Once the request has ended, a create the server never
 * tracked (refused before it began) stops polling instead of loading forever.
 */
export function useSandboxCreation(
  creationId: string | null,
  requestSettled: boolean
) {
  return useQuery({
    ...getSandboxCreationOptions({ path: { creation_id: creationId ?? '' } }),
    enabled: creationId !== null,
    retry: (failureCount) => !requestSettled && failureCount < 10,
    retryDelay: CREATION_POLL_MS,
    refetchInterval: (query) =>
      keepPolling(
        query.state.data,
        query.state.status === 'error',
        requestSettled
      )
        ? CREATION_POLL_MS
        : false,
  })
}
