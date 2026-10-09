// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { listRoutesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { RouteResponse } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { Routes as RoutesPage } from './Routes'

const listKey = listRoutesOptions().queryKey

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'Failed to list routes: route table query failed',
}
const forbidden = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires RoutesRead permission',
}

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

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/load-balancer']}>
        <BreadcrumbProvider>
          <RoutesPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** The section header always offers "Add Route"; the empty state adds a second. */
function addRouteLinks(html: string) {
  return html.split('Add Route').length - 1
}

function expectNoEmptyState(html: string) {
  expect(html).not.toContain('No routes configured')
  expect(addRouteLinks(html)).toBe(1)
}

for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed route read is not an empty list: ${String(error)}`, () => {
    const client = createClient()
    fail(client, listKey, error)
    const html = render(client)
    expect(html).toContain('Routes unavailable')
    expect(html).toContain('Retry')
    expectNoEmptyState(html)
    // The old ad-hoc alert is gone in favour of the shared read failure.
    expect(html).not.toContain('Failed to load routes data')
    if (!(error instanceof TypeError)) {
      expect(html).toContain(serverError.detail)
    }
    client.clear()
  })
}

test('forbidden route read says access denied, not "no routes"', () => {
  const client = createClient()
  fail(client, listKey, forbidden)
  const html = render(client)
  expect(html).toContain('Routes: access denied')
  expect(html).toContain(forbidden.detail)
  expectNoEmptyState(html)
  client.clear()
})

test('verified empty route list keeps the Add Route CTA', () => {
  const client = createClient()
  client.setQueryData(listKey, [] as RouteResponse[])
  const html = render(client)
  expect(html).toContain('No routes configured')
  expect(addRouteLinks(html)).toBe(2)
  expect(html).not.toContain('unavailable')
  expect(html).not.toContain('access denied')
  client.clear()
})

test('cached routes stay visible when a refresh fails', () => {
  const client = createClient()
  const cached: RouteResponse[] = [
    {
      id: 1,
      domain: 'app.example.test',
      host: '10.0.0.5',
      port: 8080,
      enabled: true,
      route_type: 'http',
      created_at: Date.parse('2026-01-01T00:00:00Z'),
      updated_at: Date.parse('2026-01-01T00:00:00Z'),
    },
  ]
  client.setQueryData(listKey, cached)
  fail(client, listKey, new TypeError('Failed to fetch'))
  const html = render(client)
  expect(html).toContain('app.example.test')
  expect(html).toContain('Routes unavailable')
  expect(html).toContain('Showing last-known data')
  expect(html).not.toContain('No routes configured')
  client.clear()
})
