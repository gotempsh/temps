// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import type { FunnelResponse } from '@/api/client/types.gen'
import {
  getFunnelMetricsQueryKey,
  getFunnelQueryKey,
  listFunnelsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { forgetDeletedFunnel, refreshSavedFunnel } from './funnel-cache'

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

test('a deleted funnel is dropped from the cache and its project lists refetch', async () => {
  const { client, keys } = seeded()

  await forgetDeletedFunnel(client, 3, 7)

  expect(client.getQueryState(keys.funnel)).toBeUndefined()
  expect(client.getQueryState(keys.metrics)).toBeUndefined()
  expect(invalidated(client, keys.firstPage)).toBe(true)
  expect(invalidated(client, keys.secondPage)).toBe(true)
  // Another funnel and another project's list are left alone.
  expect(client.getQueryData<FunnelResponse>(keys.otherFunnel)?.name).toBe(
    'Untouched'
  )
  expect(invalidated(client, keys.otherProjectList)).toBe(false)
})

test("a deleted funnel's metrics stay cached until its list refetch settles", async () => {
  const { client, keys } = seeded()
  // The list is on screen, so invalidating it refetches it; that refetch is
  // held until the test lets it finish.
  let finishList: () => void = () => {}
  let listRefetchStarted: () => void = () => {}
  const refetching = new Promise<void>((resolve) => {
    listRefetchStarted = resolve
  })
  const listObserver = new QueryObserver(client, {
    queryKey: keys.firstPage,
    queryFn: () =>
      new Promise((resolve) => {
        finishList = () =>
          resolve({ items: [], total: 0, page: 1, page_size: 20 })
        listRefetchStarted()
      }),
    staleTime: Infinity,
  })
  const unsubscribe = listObserver.subscribe(() => {})

  const forgetting = forgetDeletedFunnel(client, 3, 7)
  await refetching

  // While the list still shows the funnel, its card keeps its metrics and
  // its read: removing them now would make the card fetch them again.
  expect(client.getQueryData(keys.metrics)).toEqual({ funnel_id: 7 })
  expect(client.getQueryState(keys.funnel)).toBeDefined()
  expect(client.getQueryState(keys.firstPage)?.fetchStatus).toBe('fetching')

  finishList()
  await forgetting

  expect(client.getQueryState(keys.metrics)).toBeUndefined()
  expect(client.getQueryState(keys.funnel)).toBeUndefined()
  unsubscribe()
})
