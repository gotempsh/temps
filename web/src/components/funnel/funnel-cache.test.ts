// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import type { FunnelResponse } from '@/api/client/types.gen'
import {
  getFunnelMetricsQueryKey,
  getFunnelQueryKey,
  listFunnelsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { refreshSavedFunnel } from './funnel-cache'

const at = '2026-01-01T00:00:00Z'

function funnel(id: number, name: string): FunnelResponse {
  return {
    id,
    name,
    project_id: 3,
    is_active: true,
    created_at: at,
    updated_at: at,
    steps: [{ id: 1, order: 1, event_name: 'page_view', event_filter: [] }],
  }
}

function seeded() {
  const client = new QueryClient()
  const keys = {
    funnel: getFunnelQueryKey({ path: { project_id: 3, funnel_id: 7 } }),
    otherFunnel: getFunnelQueryKey({ path: { project_id: 3, funnel_id: 8 } }),
    firstPage: listFunnelsQueryKey({
      path: { project_id: 3 },
      query: { page: 1, page_size: 20 },
    }),
    secondPage: listFunnelsQueryKey({
      path: { project_id: 3 },
      query: { page: 2, page_size: 20 },
    }),
    otherProjectList: listFunnelsQueryKey({
      path: { project_id: 4 },
      query: { page: 1, page_size: 20 },
    }),
    metrics: getFunnelMetricsQueryKey({
      path: { project_id: 3, funnel_id: 7 },
      query: { environment_id: 2 },
    }),
  }
  client.setQueryData(keys.funnel, funnel(7, 'Old name'))
  client.setQueryData(keys.otherFunnel, funnel(8, 'Untouched'))
  for (const key of [keys.firstPage, keys.secondPage, keys.otherProjectList])
    client.setQueryData(key, { items: [], total: 0, page: 1, page_size: 20 })
  client.setQueryData(keys.metrics, { funnel_id: 7 })
  return { client, keys }
}

const invalidated = (client: QueryClient, key: readonly unknown[]) =>
  client.getQueryState(key)?.isInvalidated ?? false

test('a saved funnel replaces its cached read and refreshes its lists and metrics', () => {
  const { client, keys } = seeded()

  refreshSavedFunnel(client, 3, funnel(7, 'New name'))

  expect(client.getQueryData<FunnelResponse>(keys.funnel)?.name).toBe(
    'New name'
  )
  expect(invalidated(client, keys.firstPage)).toBe(true)
  expect(invalidated(client, keys.secondPage)).toBe(true)
  expect(invalidated(client, keys.metrics)).toBe(true)
  // Other funnels and other projects' lists are left alone.
  expect(invalidated(client, keys.otherFunnel)).toBe(false)
  expect(client.getQueryData<FunnelResponse>(keys.otherFunnel)?.name).toBe(
    'Untouched'
  )
  expect(invalidated(client, keys.otherProjectList)).toBe(false)
})

test('a bare operation name, the key the pages used before, matches no generated query', () => {
  const { client, keys } = seeded()

  void client.invalidateQueries({ queryKey: ['listFunnels'] })
  void client.invalidateQueries({ queryKey: ['getFunnel'] })

  expect(invalidated(client, keys.firstPage)).toBe(false)
  expect(invalidated(client, keys.funnel)).toBe(false)
})
