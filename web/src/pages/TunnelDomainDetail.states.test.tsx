// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { getIngressHostnameOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ConnectorView, HostnameView } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import { TunnelDomainDetail } from './TunnelDomainDetail'

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function render(client: QueryClient, hostname: string) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <BreadcrumbProvider>
        <MemoryRouter
          initialEntries={[`/domains/tunnel/${encodeURIComponent(hostname)}`]}
        >
          <Routes>
            <Route
              path="/domains/tunnel/:hostname"
              element={<TunnelDomainDetail />}
            />
          </Routes>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
}

const connector = {
  id: 1,
  name: 'edge',
  provider: 'cloudflare',
  domain: 'example.com',
  status: 'error',
  last_error: 'Cloudflare refused the tunnel token',
  dns_target: 'abc.cfargotunnel.com',
  nodes: [
    {
      node_id: 7,
      node_name: 'worker-1',
      state: 'ready',
      last_error: null,
      ready_connections: 4,
    },
  ],
} as unknown as ConnectorView

test('a tunnel zone is a record page with its verdict, facts and error', () => {
  const client = createClient()
  client.setQueryData(allTunnelsOptions().queryKey, [connector])
  const html = render(client, '*.example.com')
  expect(html.match(/<h1/g)?.length).toBe(1)
  expect(html).toContain('*.example.com')
  expect(html).toContain('Failed')
  expect(html).toContain('Workers connected')
  expect(html).toContain('1 of 1')
  expect(html).toContain('abc.cfargotunnel.com')
  expect(html).toContain('Cloudflare refused the tunnel token')
  expect(html).toContain('Back to Domains')
  client.clear()
})

test('a zone no tunnel covers says so and links back', () => {
  const client = createClient()
  client.setQueryData(allTunnelsOptions().queryKey, [])
  const html = render(client, '*.other.dev')
  expect(html).toContain('No tunnel covers other.dev')
  expect(html).toContain('Back to Domains')
  client.clear()
})

test('a published hostname shows it is live and can be opened', () => {
  const client = createClient()
  const view = {
    hostname: 'app.example.com',
    state: 'published',
    detail: 'Published through tunnel edge.',
    mode: 'automatic',
    chosen_connector_id: null,
    candidates: [],
    connector_name: 'edge',
    served_by_workers: true,
    published_at: '2026-10-22T00:00:00Z',
  } as unknown as HostnameView
  client.setQueryData(
    getIngressHostnameOptions({ path: { hostname: 'app.example.com' } })
      .queryKey,
    view
  )
  const html = render(client, 'app.example.com')
  expect(html.match(/<h1/g)?.length).toBe(1)
  expect(html).toContain('Live')
  expect(html).toContain('Open')
  expect(html).toContain('Yes, on a worker')
  client.clear()
})
