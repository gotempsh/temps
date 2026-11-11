// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { restoreSourcesQuery } from './restore-queries'

installBrowserWindow()
afterEach(() => undefined)

async function observeSources(status: number) {
  const api = mockApi({
    'GET /backups/s3-sources': () =>
      jsonResponse({ title: 'Problem', status, detail: 'injected' }, status),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const observer = new QueryObserver(client, {
    ...restoreSourcesQuery(),
    retry: false,
  })
  const unsubscribe = observer.subscribe(() => undefined)
  await observer.refetch()
  return { client, api, unsubscribe }
}

test('a refused source list is not reported by the shell, the page falls back to its own backups', async () => {
  const { client, api, unsubscribe } = await observeSources(403)
  expect(client.getQueryCache().findAll()[0]?.state.status).toBe('error')
  expect(collectRouteQueryFailures(client)).toEqual([])
  unsubscribe()
  api.restore()
})

test('a real failure of the source list is explained by the page itself, so the shell stays quiet too', async () => {
  const { client, api, unsubscribe } = await observeSources(500)
  expect(client.getQueryCache().findAll()[0]?.state.status).toBe('error')
  expect(collectRouteQueryFailures(client)).toEqual([])
  unsubscribe()
  api.restore()
})
