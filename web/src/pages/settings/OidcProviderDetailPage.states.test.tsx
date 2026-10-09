// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, beforeAll, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listOidcProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import { OidcProviderDetailPage } from './OidcProviderDetailPage'

const providersKey = listOidcProvidersOptions().queryKey

// The page derives the OIDC redirect URI from `window.location` on render.
const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window')
beforeAll(() => {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { location: { origin: 'https://console.example.test' } },
  })
})
afterAll(() => {
  if (previousWindow)
    Object.defineProperty(globalThis, 'window', previousWindow)
  else Reflect.deleteProperty(globalThis, 'window')
})

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
      <MemoryRouter initialEntries={['/settings/auth/oidc/7']}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/settings/auth/oidc/:providerId"
              element={<OidcProviderDetailPage />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a successful provider list that lacks the id shows not found', () => {
  const client = createClient()
  client.setQueryData(providersKey, [])
  const html = render(client)
  expect(html).toContain('Provider not found')
  expect(html).not.toContain('unavailable')
  expect(html).toContain('Back to authentication')
  client.clear()
})

const serverDetail = 'Database pool exhausted while listing OIDC providers'
for (const error of [
  { title: 'Internal Server Error', status: 500, detail: serverDetail },
  new TypeError('Failed to fetch'),
]) {
  test(`a failed provider list read is not a missing provider: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    fail(client, providersKey, error)
    const html = render(client)
    expect(html).toContain('SSO provider unavailable')
    expect(html).not.toContain('Provider not found')
    expect(html).not.toContain('Failed to fetch')
    expect(html).not.toContain('data is undefined')
    expect(html).toContain('Retry')
    expect(html).toContain('Back to authentication')
    if (!(error instanceof TypeError)) expect(html).toContain(serverDetail)
    client.clear()
  })
}

test('a forbidden provider list read shows access denied with the reason', () => {
  const client = createClient()
  fail(client, providersKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires settings read permission',
  })
  const html = render(client)
  expect(html).toContain('SSO provider: access denied')
  expect(html).toContain('Requires settings read permission')
  expect(html).not.toContain('Provider not found')
  expect(html).toContain('Back to authentication')
  client.clear()
})
