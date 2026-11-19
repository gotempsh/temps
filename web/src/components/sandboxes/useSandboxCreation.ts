// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getSandboxCreationOptions } from '@/api/client/@tanstack/react-query.gen'
import { CREATION_POLL_MS, keepPolling } from './sandbox-creation'

/**
 * Polls the steps of the create sent with `creationId` while it runs. A 404
 * before the server has started tracking it is retried while the create
 * request is still in flight (`requestSettled` false): the request and the
 * first poll race. A 404 otherwise means the server does not track the
 * create and stops polling; a network or server error keeps polling (see
 * `keepPolling`).
 */
export function useSandboxCreation(
  creationId: string | null,
  requestSettled: boolean
) {
  return useQuery({
    ...getSandboxCreationOptions({ path: { creation_id: creationId ?? '' } }),
    enabled: creationId !== null,
    // No hidden retries: every read's outcome reaches `keepPolling`, which
    // decides from it. Retries would hold a 404 (the server lost the
    // create) for seconds before anyone saw it.
    retry: false,
    refetchInterval: (query) =>
      keepPolling(
        query.state.data,
        query.state.status === 'error' ? query.state.error : null,
        requestSettled
      )
        ? CREATION_POLL_MS
        : false,
  })
}
