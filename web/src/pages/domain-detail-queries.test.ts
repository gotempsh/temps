// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  getDomainByIdQueryKey,
  getDomainOrderQueryKey,
  getHttpChallengeDebugQueryKey,
  getPublicIpQueryKey,
  getProjectsQueryKey,
  listDnsProvidersQueryKey,
  listRenewalAttemptsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import {
  isDomainDetailQuery,
  refreshDomainDetail,
} from './domain-detail-queries'

test('matches the renewal attempts query for any page', () => {
  expect(
    isDomainDetailQuery(
      listRenewalAttemptsQueryKey({
        path: { domain: 'app.example.test' },
        query: { page: 2, page_size: 10 },
      })
    )
  ).toBe(true)
})

test('matches every query the domain detail page loads', () => {
  const keys = [
    getDomainByIdQueryKey({ path: { domain: 1 } }),
    getDomainOrderQueryKey({ path: { domain_id: 1 } }),
    getHttpChallengeDebugQueryKey({
      path: { domain: 'app.example.test' },
      query: { challenge_type: 'http-01' },
    } as never),
    getPublicIpQueryKey(),
    listDnsProvidersQueryKey(),
  ]
  for (const key of keys) expect(isDomainDetailQuery(key)).toBe(true)
})

test('ignores unrelated and malformed keys', () => {
  expect(isDomainDetailQuery(getProjectsQueryKey())).toBe(false)
  expect(isDomainDetailQuery(['getDomainById'])).toBe(false)
  expect(isDomainDetailQuery([])).toBe(false)
  expect(isDomainDetailQuery([null])).toBe(false)
  expect(isDomainDetailQuery(undefined)).toBe(false)
})

// A failed validation is returned as an error, but the server has already
// persisted a failed renewal attempt. The page wires `onError` to a refresh.
test('refreshDomainDetail surfaces a failed attempt after a rejected mutation', async () => {
  const { QueryClient, MutationObserver } =
    await import('@tanstack/react-query')
  const client = new QueryClient()
  const historyKey = listRenewalAttemptsQueryKey({
    path: { domain: 'app.example.test' },
    query: { page: 1, page_size: 10 },
  })
  let serverAttempts: string[] = []
  const history = client.getQueryCache().build(client, {
    queryKey: historyKey,
    queryFn: async () => ({ attempts: [...serverAttempts] }),
  })
  // Observe it so invalidation refetches, as the mounted timeline does.
  const observer = new (await import('@tanstack/react-query')).QueryObserver(
    client,
    {
      queryKey: historyKey,
      queryFn: async () => ({ attempts: [...serverAttempts] }),
    }
  )
  const unsubscribe = observer.subscribe(() => {})
  await observer.refetch()
  expect(history.state.data).toEqual({ attempts: [] })

  const mutation = new MutationObserver(client, {
    mutationFn: async () => {
      serverAttempts = ['failed']
      throw new Error('challenge validation failed')
    },
    onError: () => refreshDomainDetail(client),
  })
  await mutation.mutate().catch(() => {})

  expect(history.state.data).toEqual({ attempts: ['failed'] })
  unsubscribe()
})

test('refreshDomainDetail drops the order query when asked to clear it', async () => {
  const { QueryClient } = await import('@tanstack/react-query')
  const client = new QueryClient()
  const orderKey = getDomainOrderQueryKey({ path: { domain_id: 1 } })
  client.setQueryData(orderKey, { id: 1 })
  await refreshDomainDetail(client, { clearOrder: orderKey })
  expect(client.getQueryData(orderKey)).toBeUndefined()
})
