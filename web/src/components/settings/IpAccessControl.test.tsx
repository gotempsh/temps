// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { listIpAccessControlOptions } from '@/api/client/@tanstack/react-query.gen'
import { IpAccessControl } from './IpAccessControl'

const key = listIpAccessControlOptions().queryKey

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <IpAccessControl />
    </QueryClientProvider>
  )
}

function fail(client: QueryClient, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey: key })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

test('a refused rule read is access denied, not "no blocked IPs"', () => {
  const client = createClient()
  fail(client, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires load_balancer:read permission',
  })
  const html = render(client)
  expect(html).toContain('IP access control rules: access denied')
  expect(html).toContain('Requires load_balancer:read permission')
  expect(html).not.toContain('No blocked IPs configured')
  expect(html).not.toContain('Edit rule')
  expect(html).not.toContain('Delete rule')
  // The write entry point is disabled alongside the failed read.
  expect(
    /<button[^>]*>(?:(?!<\/button>).)*Add IP Rule/s.exec(html)?.[0]
  ).toContain('disabled=""')
  client.clear()
})

test('a verified-empty rule list still shows the empty sections', () => {
  const client = createClient()
  client.setQueryData(key, [])
  const html = render(client)
  expect(html).toContain('No blocked IPs configured')
  expect(html).not.toContain('access denied')
  client.clear()
})
