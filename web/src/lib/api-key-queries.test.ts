// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import {
  getAiHarnessStatusQueryKey,
  getApiKeyQueryKey,
  listApiKeysQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { invalidateApiKeyQueries } from './api-key-queries'

describe('invalidateApiKeyQueries', () => {
  test('marks the key list and the AI harness status stale', async () => {
    const client = new QueryClient()
    const listKey = listApiKeysQueryKey({ query: { page: 1, page_size: 20 } })
    const statusKey = getAiHarnessStatusQueryKey()
    const unrelatedKey = getApiKeyQueryKey({ path: { id: 7 } })
    client.setQueryData(listKey, { api_keys: [], total: 0 })
    client.setQueryData(statusKey, { status: 'missing' })
    client.setQueryData(unrelatedKey, { id: 7 })

    await invalidateApiKeyQueries(client)

    expect(client.getQueryState(listKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(statusKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(unrelatedKey)?.isInvalidated).toBe(false)
  })
})
