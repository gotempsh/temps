// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  getGitProviderOptions,
  getProviderConnectionsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import GitProviderDetail from './GitProviderDetail'

const providerKey = getGitProviderOptions({ path: { provider_id: 3 } }).queryKey
const connectionsKey = getProviderConnectionsOptions({
  path: { provider_id: 3 },
}).queryKey
const NOT_FOUND = 'Git Provider Not Found'

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
      <MemoryRouter initialEntries={['/git-providers/3']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/git-providers/:id" element={<GitProviderDetail />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('only a verified 404 shows the provider as not found', () => {
  const client = createClient()
  fail(client, providerKey, {
    title: 'Not Found',
    status: 404,
    detail: 'Git provider 3 not found',
  })
  const html = render(client)
  expect(html).toContain(NOT_FOUND)
  expect(html).not.toContain('unavailable')
  expect(html).toContain('Back')
  client.clear()
})

const serverDetail = 'Database pool exhausted while reading git provider 3'
for (const error of [
  { title: 'Internal Server Error', status: 500, detail: serverDetail },
  new TypeError('Failed to fetch'),
]) {
  test(`a failed provider read is not a missing provider: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    fail(client, providerKey, error)
    const html = render(client)
    expect(html).toContain('Git provider unavailable')
    expect(html).not.toContain(NOT_FOUND)
    expect(html).not.toContain('Failed to fetch')
    expect(html).not.toContain('data is undefined')
    expect(html).toContain('Retry')
    expect(html).toContain('Back')
    if (!(error instanceof TypeError)) expect(html).toContain(serverDetail)
    client.clear()
  })
}

test('a forbidden provider read shows access denied with the reason', () => {
  const client = createClient()
  fail(client, providerKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires git providers read permission',
  })
  const html = render(client)
  expect(html).toContain('Git provider: access denied')
  expect(html).toContain('Requires git providers read permission')
  expect(html).not.toContain(NOT_FOUND)
  client.clear()
})

test('a failed refresh keeps the cached provider and says it is stale', () => {
  const client = createClient()
  client.setQueryData(providerKey, {
    id: 3,
    name: 'Example GitLab',
    provider_type: 'gitlab',
    auth_method: 'token',
    base_url: 'https://gitlab.example.test',
    is_active: true,
    is_default: false,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  })
  client.setQueryData(connectionsKey, [])
  fail(client, providerKey, new TypeError('Failed to fetch'))
  const html = render(client)
  expect(html).toContain('Example GitLab')
  expect(html).toContain('Git provider unavailable')
  expect(html).toContain('Showing last-known data')
  expect(html).not.toContain(NOT_FOUND)
  client.clear()
})
