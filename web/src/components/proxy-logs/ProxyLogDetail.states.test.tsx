// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  getProxyLogByIdOptions,
  getProxyLogByRequestIdOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProxyLogDetail } from './ProxyLogDetail'

const requestId = 'req-0001'
const byRequestIdKey = getProxyLogByRequestIdOptions({
  path: { request_id: requestId },
  query: { timestamp: undefined, project_id: undefined },
}).queryKey
const byIdKey = getProxyLogByIdOptions({
  path: { id: 42 },
  query: { timestamp: undefined, project_id: undefined },
}).queryKey

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}
function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}
function render(client: QueryClient, logId = requestId) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProxyLogDetail logId={logId} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

for (const [logId, key] of [
  [requestId, byRequestIdKey],
  ['42', byIdKey],
] as const) {
  test(`verified 404 shows proxy log not found (${logId})`, () => {
    const client = createClient()
    fail(client, key, { title: 'Not Found', status: 404 })
    const html = render(client, logId)
    expect(html).toContain('Proxy log not found')
    expect(html).not.toContain('Proxy log unavailable')
    client.clear()
  })

  for (const error of [
    {
      title: 'Internal Server Error',
      status: 500,
      detail: 'log store query timed out',
    },
    new TypeError('Failed to fetch'),
  ]) {
    test(`failed proxy log read is not a missing log (${logId}): ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
      const client = createClient()
      fail(client, key, error)
      const html = render(client, logId)
      expect(html).toContain('Proxy log unavailable')
      expect(html).not.toContain('Proxy log not found')
      expect(html).toContain('Retry')
      if (!(error instanceof TypeError)) {
        expect(html).toContain('log store query timed out')
      }
      client.clear()
    })
  }

  test(`forbidden proxy log read shows access denied (${logId})`, () => {
    const client = createClient()
    fail(client, key, {
      title: 'Forbidden',
      status: 403,
      detail: 'Requires proxy logs read permission',
    })
    const html = render(client, logId)
    expect(html).toContain('Proxy log: access denied')
    expect(html).toContain('Requires proxy logs read permission')
    expect(html).not.toContain('Proxy log not found')
    client.clear()
  })
}
