// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  getConnectionOptions,
  getGitProviderOptions,
} from '@/api/client/@tanstack/react-query.gen'
import GitConnectionDetail from './GitConnectionDetail'

const providerKey = getGitProviderOptions({ path: { provider_id: 3 } }).queryKey
const connectionKey = getConnectionOptions({
  path: { connection_id: 9 },
}).queryKey
const NOT_FOUND = 'Git Connection Not Found'

const provider = {
  id: 3,
  name: 'Example GitLab',
  provider_type: 'gitlab',
  auth_method: 'token',
  base_url: 'https://gitlab.example.test',
  is_active: true,
  is_default: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
const connection = {
  id: 9,
  provider_id: 3,
  account_name: 'example-org',
  account_type: 'Organization',
  consecutive_health_failures: 0,
  has_authenticated_credentials: true,
  health_status: 'healthy',
  is_active: true,
  is_expired: false,
  synced_repository_count: 0,
  syncing: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
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
      <MemoryRouter initialEntries={['/git-providers/3/connections/9']}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/git-providers/:id/connections/:connectionId"
              element={<GitConnectionDetail />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('only a verified 404 shows the connection as not found', () => {
  const client = createClient()
  client.setQueryData(providerKey, provider)
  fail(client, connectionKey, {
    title: 'Not Found',
    status: 404,
    detail: 'Git connection 9 not found',
  })
  const html = render(client)
  expect(html).toContain(NOT_FOUND)
  expect(html).not.toContain('unavailable')
  expect(html).toContain('Back')
  client.clear()
})

const serverDetail = 'Database pool exhausted while reading git connection 9'
for (const error of [
  { title: 'Internal Server Error', status: 500, detail: serverDetail },
  new TypeError('Failed to fetch'),
]) {
  test(`a failed connection read is not a missing connection: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    client.setQueryData(providerKey, provider)
    fail(client, connectionKey, error)
    const html = render(client)
    expect(html).toContain('Git connection unavailable')
    expect(html).not.toContain(NOT_FOUND)
    expect(html).not.toContain('Failed to fetch')
    expect(html).not.toContain('data is undefined')
    expect(html).toContain('Retry')
    expect(html).toContain('Back')
    if (!(error instanceof TypeError)) expect(html).toContain(serverDetail)
    client.clear()
  })
}

test('a forbidden connection read shows access denied with the reason', () => {
  const client = createClient()
  client.setQueryData(providerKey, provider)
  fail(client, connectionKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires git connections read permission',
  })
  const html = render(client)
  expect(html).toContain('Git connection: access denied')
  expect(html).toContain('Requires git connections read permission')
  expect(html).not.toContain(NOT_FOUND)
  client.clear()
})

test('a failed provider read is not a missing connection', () => {
  const client = createClient()
  client.setQueryData(connectionKey, connection)
  fail(client, providerKey, {
    title: 'Internal Server Error',
    status: 500,
    detail: serverDetail,
  })
  const html = render(client)
  expect(html).toContain('Git provider unavailable')
  expect(html).toContain(serverDetail)
  expect(html).not.toContain(NOT_FOUND)
  client.clear()
})

test('a verified 404 for the provider shows the connection as not found', () => {
  const client = createClient()
  client.setQueryData(connectionKey, connection)
  fail(client, providerKey, { title: 'Not Found', status: 404 })
  expect(render(client)).toContain(NOT_FOUND)
  client.clear()
})
