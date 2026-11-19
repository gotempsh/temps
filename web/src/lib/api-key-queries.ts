// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient } from '@tanstack/react-query'
import {
  getAiHarnessStatusQueryKey,
  listApiKeysQueryKey,
} from '@/api/client/@tanstack/react-query.gen'

/**
 * Refresh every cached view derived from the caller's API keys after one is
 * created, activated, deactivated, edited or deleted: the key list and the AI
 * harness status. The harness status stops polling once it is `missing` or
 * `connected`, so without this a key change (reactivating the harness key,
 * deleting it) left the setup checklist and its banner showing a stale state.
 */
export function invalidateApiKeyQueries(queryClient: QueryClient) {
  return Promise.all([
    queryClient.invalidateQueries({ queryKey: listApiKeysQueryKey() }),
    queryClient.invalidateQueries({ queryKey: getAiHarnessStatusQueryKey() }),
  ])
}
