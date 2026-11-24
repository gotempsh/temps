// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listOidcProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import { AuthSettingsPage } from './AuthSettingsPage'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires settings:write permission',
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
          <AuthSettingsPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const providersKey = listOidcProvidersOptions().queryKey

test('a refused provider list is not rendered as "no SSO providers configured"', () => {
  const client = createClient()
  fail(client, providersKey, refused)
  const html = render(client)
  expect(html).toContain('SSO providers: access denied')
  expect(html).toContain('Requires settings:write permission')
  expect(html).not.toContain('No SSO providers configured')
  expect(html).not.toContain('Add SSO provider')
  client.clear()
})

test('a verified-empty provider list still onboards the operator', () => {
  const client = createClient()
  client.setQueryData(providersKey, [])
  const html = render(client)
  expect(html).toContain('No SSO providers configured')
  expect(html).toContain('Add SSO provider')
  expect(html).not.toContain('access denied')
  client.clear()
})
