// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listOnDemandCertsOptions } from '@/lib/on-demand-certs'
import { Certificates } from './Certificates'

const certsKey = listOnDemandCertsOptions({ page: 1, page_size: 20 }).queryKey
const settingsKey = ['platform-settings']

function createClient() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData(certsKey, {
    certs: [],
    total: 0,
    page: 1,
    page_size: 20,
  })
  return client
}

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/certificates']}>
        <BreadcrumbProvider>
          <Certificates />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('enabled: no instruction to enable, links to the settings', () => {
  const client = createClient()
  client.setQueryData(settingsKey, { on_demand_tls: { enabled: true } })
  const html = render(client)
  expect(html).toContain('No certificate attempts yet')
  expect(html).toContain('On-demand TLS is on')
  expect(html).not.toContain('Enable on-demand TLS in settings')
  expect(html).toContain('href="/settings/on-demand-tls"')
})

test('disabled: says it is off and offers the switch', () => {
  const client = createClient()
  client.setQueryData(settingsKey, { on_demand_tls: { enabled: false } })
  const html = render(client)
  expect(html).toContain('On-demand TLS is off')
  expect(html).toContain('Turn on on-demand TLS')
  expect(html).toContain('href="/settings/on-demand-tls"')
})

test('settings read forbidden: neutral copy, never claims it is off', () => {
  const client = createClient()
  client
    .getQueryCache()
    .build(client, { queryKey: settingsKey })
    .setState({
      status: 'error',
      error: Object.assign(new Error('Forbidden'), { status: 403 }),
      fetchStatus: 'idle',
    })
  const html = render(client)
  expect(html).toContain('No certificate attempts yet')
  expect(html).not.toContain('On-demand TLS is off')
  expect(html).not.toContain('Turn on on-demand TLS')
  expect(html).not.toContain('href="/settings/on-demand-tls"')
})
