// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient } from '@tanstack/react-query'

/** The gateway page's per-key model inventory query. */
export const providerModelInventoryKey = (providerKeyId: number) =>
  ['providerModelInventory', providerKeyId] as const

/**
 * After a key's models change (refresh, add, enable, remove), every cached
 * view of them refetches: the gateway page's inventory and the generated
 * `getProviderKey` query the model pickers read. Without the second, a
 * picker opened within its stale time keeps the old list, and a model the
 * refresh just discovered cannot be chosen. All keys when no id is given.
 */
export function invalidateProviderModels(
  queryClient: QueryClient,
  providerKeyId?: number
): Promise<unknown> {
  return Promise.all([
    queryClient.invalidateQueries({
      queryKey:
        providerKeyId === undefined
          ? ['providerModelInventory']
          : providerModelInventoryKey(providerKeyId),
    }),
    queryClient.invalidateQueries({
      predicate: (query) => {
        const head = query.queryKey[0] as
          { _id?: unknown; path?: { id?: unknown } } | undefined
        return (
          typeof head === 'object' &&
          head !== null &&
          head._id === 'getProviderKey' &&
          (providerKeyId === undefined || head.path?.id === providerKeyId)
        )
      },
    }),
  ])
}
