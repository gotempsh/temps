// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { listApiKeysOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ApiKeyListResponse } from '@/api/client'
import ApiKeys from './ApiKeys'

const listKey = listApiKeysOptions({
  query: { page: 1, page_size: 100 },
}).queryKey

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'Failed to list API keys for user 7: connection pool timed out',
}
const forbidden = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires ApiKeysRead permission',
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
      <MemoryRouter initialEntries={['/settings/keys']}>
        <ApiKeys />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function expectNoEmptyState(html: string) {
  expect(html).not.toContain('No API keys yet')
  expect(html).not.toContain('Create Your First API Key')
}

for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed API key read is not an empty list: ${String(error)}`, () => {
    const client = createClient()
    fail(client, listKey, error)
    const html = render(client)
    expect(html).toContain('API keys unavailable')
    expect(html).toContain('Retry')
    expectNoEmptyState(html)
    if (!(error instanceof TypeError)) {
      expect(html).toContain('Server response:')
      expect(html).toContain(serverError.detail)
    }
    client.clear()
  })
}

test('forbidden API key read says access denied, not "no keys"', () => {
  const client = createClient()
  fail(client, listKey, forbidden)
  const html = render(client)
  expect(html).toContain('API keys: access denied')
  expect(html).toContain(forbidden.detail)
  expectNoEmptyState(html)
  client.clear()
})

test('verified empty API key list keeps the create CTA', () => {
  const client = createClient()
  const empty: ApiKeyListResponse = { api_keys: [], total: 0 }
  client.setQueryData(listKey, empty)
  const html = render(client)
  expect(html).toContain('No API keys yet')
  expect(html).toContain('Create Your First API Key')
  expect(html).not.toContain('unavailable')
  expect(html).not.toContain('access denied')
  client.clear()
})

test('cached API keys stay visible when a refresh fails', () => {
  const client = createClient()
  const cached: ApiKeyListResponse = {
    api_keys: [
      {
        id: 1,
        name: 'ci-deploy-key',
        key_prefix: 'tk_abc',
        role_type: 'admin',
        is_active: true,
        created_at: '2026-01-01T00:00:00Z',
      } as ApiKeyListResponse['api_keys'][number],
    ],
    total: 1,
  }
  client.setQueryData(listKey, cached)
  fail(client, listKey, serverError)
  const html = render(client)
  expect(html).toContain('ci-deploy-key')
  expect(html).toContain('API keys unavailable')
  expect(html).toContain('Showing last-known data')
  expectNoEmptyState(html)
  client.clear()
})
