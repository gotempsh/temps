// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import { getProviderKeyQueryKey } from '@/api/client/@tanstack/react-query.gen'
import {
  invalidateProviderModels,
  providerModelInventoryKey,
} from './provider-model-cache'

function seeded() {
  const client = new QueryClient()
  for (const id of [1, 2]) {
    client.setQueryData(providerModelInventoryKey(id), { id })
    client.setQueryData(getProviderKeyQueryKey({ path: { id } }), { id })
  }
  client.setQueryData(['providerKeys'], [])
  return client
}

const invalidated = (client: QueryClient, key: readonly unknown[]) =>
  client.getQueryState(key)?.isInvalidated

describe('invalidateProviderModels', () => {
  it("refreshes the picker's cached key along with the inventory", async () => {
    const client = seeded()
    await invalidateProviderModels(client, 1)
    expect(invalidated(client, providerModelInventoryKey(1))).toBe(true)
    expect(
      invalidated(client, getProviderKeyQueryKey({ path: { id: 1 } }))
    ).toBe(true)
    // Other keys and unrelated queries are left alone.
    expect(invalidated(client, providerModelInventoryKey(2))).toBe(false)
    expect(
      invalidated(client, getProviderKeyQueryKey({ path: { id: 2 } }))
    ).toBe(false)
    expect(invalidated(client, ['providerKeys'])).toBe(false)
  })

  it('refreshes every key when none is named', async () => {
    const client = seeded()
    await invalidateProviderModels(client)
    for (const id of [1, 2]) {
      expect(invalidated(client, providerModelInventoryKey(id))).toBe(true)
      expect(
        invalidated(client, getProviderKeyQueryKey({ path: { id } }))
      ).toBe(true)
    }
  })
})
