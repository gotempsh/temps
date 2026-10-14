// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listTeamsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Teams } from './Teams'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires users:read permission',
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
      <MemoryRouter>
        <BreadcrumbProvider>
          <Teams />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const teamsKey = listTeamsOptions({
  query: { page: 1, page_size: 100 },
}).queryKey

test('a refused teams read shows the server reason instead of an unreachable API', () => {
  const client = createClient()
  fail(client, teamsKey, refused)
  const html = render(client)
  expect(html).toContain('Teams: access denied')
  expect(html).toContain('Requires users:read permission')
  expect(html).not.toContain('did not respond')
  expect(html).not.toContain('No teams yet')
  client.clear()
})

test('a failed teams read is not an empty team list', () => {
  const client = createClient()
  fail(client, teamsKey, {
    title: 'Internal Server Error',
    status: 500,
    detail: 'teams table unavailable',
  })
  const html = render(client)
  expect(html).toContain('Teams unavailable')
  expect(html).toContain('teams table unavailable')
  expect(html).not.toContain('No teams yet')
  client.clear()
})
