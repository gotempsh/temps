// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { client as apiClient } from '@/api/client/client.gen'
import { getApiKeyOptions } from '@/api/client/@tanstack/react-query.gen'
import ApiKeyDetail from './ApiKeyDetail'

const apiKeyKey = getApiKeyOptions({ path: { id: 1 } }).queryKey
const NOT_FOUND = 'API key not found'

const originalConfig = apiClient.getConfig()
afterEach(() => apiClient.setConfig(originalConfig))

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
      <MemoryRouter initialEntries={['/settings/keys/1']}>
        <Routes>
          <Route path="/settings/keys/:id" element={<ApiKeyDetail />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('only a verified 404 shows the API key as not found', () => {
  const client = createClient()
  fail(client, apiKeyKey, {
    title: 'Not Found',
    status: 404,
    detail: 'API key 1 not found',
  })
  const html = render(client)
  expect(html).toContain(NOT_FOUND)
  expect(html).not.toContain('unavailable')
  expect(html).toContain('Back to API Keys')
  client.clear()
})

const serverDetail = 'Database pool exhausted while reading API key 1'
for (const error of [
  { title: 'Internal Server Error', status: 500, detail: serverDetail },
  new TypeError('Failed to fetch'),
]) {
  test(`a failed API key read is not a missing key: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    fail(client, apiKeyKey, error)
    const html = render(client)
    expect(html).toContain('API key unavailable')
    expect(html).not.toContain(NOT_FOUND)
    expect(html).not.toContain('Failed to load API key')
    expect(html).not.toContain('Failed to fetch')
    expect(html).not.toContain('data is undefined')
    expect(html).toContain('Retry')
    expect(html).toContain('Back to API Keys')
    if (!(error instanceof TypeError)) expect(html).toContain(serverDetail)
    client.clear()
  })
}

test('a forbidden API key read shows access denied with the reason', () => {
  const client = createClient()
  fail(client, apiKeyKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires API keys read permission',
  })
  const html = render(client)
  expect(html).toContain('API key: access denied')
  expect(html).toContain('Requires API keys read permission')
  expect(html).not.toContain(NOT_FOUND)
  client.clear()
})

test('the API key query rejects with the server problem, not undefined data', async () => {
  // The SDK resolves `{ error }` instead of throwing unless asked to. The
  // page's query must surface the server's Problem Details as the error.
  const problem = {
    title: 'Internal Server Error',
    status: 500,
    detail: serverDetail,
  }
  apiClient.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(async () => Response.json(problem, { status: 500 }), {
      preconnect: fetch.preconnect,
    }),
  })
  // Built after `setConfig`: the generated query key carries the base URL,
  // exactly as the page builds it while rendering.
  const options = getApiKeyOptions({ path: { id: 1 } })
  const client = createClient()
  await expect(client.fetchQuery(options)).rejects.toMatchObject(problem)
  // The page reads exactly this query, so it shows the server's reason.
  const html = render(client)
  expect(html).toContain('API key unavailable')
  expect(html).toContain(serverDetail)
  expect(html).not.toContain('data is undefined')
  client.clear()
})
