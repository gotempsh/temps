// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { client } from '@/api/client/client.gen'
import { attachHttpStatus } from '@/lib/http-error-status'
import {
  DNS_PROVIDERS_QUERY_KEY,
  dnsProvidersQueryOptions,
} from '@/lib/dns-providers-query'
import { DnsProviders } from './DnsProviders'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires dns_providers:read permission',
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

function render(client: QueryClient, role?: string) {
  const user: UserResponse | null = role
    ? {
        id: 3,
        name: 'Example User',
        username: 'example',
        avatar_url: '',
        mfa_enabled: false,
        role,
      }
    : null
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <DnsProviders />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const providersKey = DNS_PROVIDERS_QUERY_KEY

test('a refused DNS provider read says access is denied, not that support is needed', () => {
  const client = createClient()
  fail(client, providersKey, refused)
  const html = render(client)
  expect(html).toContain('DNS providers: access denied')
  expect(html).toContain('Requires dns_providers:read permission')
  expect(html).not.toContain('contact support')
  expect(html).not.toContain('No DNS providers found')
  client.clear()
})

test('a verified-empty provider list still shows the empty state', () => {
  const client = createClient()
  client.setQueryData(providersKey, [])
  const html = render(client)
  expect(html).toContain('No DNS providers found')
  expect(html).not.toContain('access denied')
  client.clear()
})

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
})

test('a real HTTP 403 reaches the page as access denied with the server reason', async () => {
  // The console installs this once at startup (App.tsx).
  if (!client.interceptors.error.exists(attachHttpStatus)) {
    client.interceptors.error.use(attachHttpStatus)
  }
  // bun has no document base, so a relative API URL cannot become a Request.
  const previousBaseUrl = client.getConfig().baseUrl
  client.setConfig({ baseUrl: 'http://console.example.test/api' })
  globalThis.fetch = (async () =>
    new Response(
      JSON.stringify({
        title: 'Forbidden',
        status: 403,
        detail: 'Requires dns_providers:read permission',
      }),
      { status: 403, headers: { 'Content-Type': 'application/json' } }
    )) as unknown as typeof fetch

  const client_ = createClient()
  const error = await client_.fetchQuery(dnsProvidersQueryOptions()).then(
    () => undefined,
    (e: unknown) => e
  )
  expect(error).toBeDefined()
  expect(String(error)).not.toContain('data is undefined')

  client.setConfig({ baseUrl: previousBaseUrl })

  fail(client_, providersKey, error)
  const html = render(client_)
  expect(html).toContain('DNS providers: access denied')
  expect(html).toContain('Requires dns_providers:read permission')
  client_.clear()
})

test('a non-admin is offered no add-provider action and is told why', () => {
  const client = createClient()
  client.setQueryData(providersKey, [])
  const html = render(client, 'user')
  expect(html).toContain('data-admin-only-note')
  expect(html).toContain('Only administrators can add, test or delete')
  expect(html).not.toContain('Add DNS provider')
  client.clear()
})

test('an administrator gets the add-provider action and no note', () => {
  const client = createClient()
  client.setQueryData(providersKey, [])
  const html = render(client, 'admin')
  expect(html).not.toContain('data-admin-only-note')
  expect(html).toContain('Add DNS provider')
  client.clear()
})

test('a refused provider read offers no Retry', () => {
  const client = createClient()
  fail(client, providersKey, refused)
  const html = render(client, 'user')
  expect(html).toContain('DNS providers: access denied')
  expect(html).not.toContain('Retry')
  client.clear()
})

test('a downgraded role loses Test and Delete on already-fetched providers', () => {
  const client = createClient()
  client.setQueryData(providersKey, [
    {
      id: 4,
      name: 'Example DNS',
      provider_type: 'cloudflare',
      description: null,
      is_active: true,
      flat_hostnames_supported: false,
      credentials: {},
      created_at: '2026-01-01T00:00:00Z',
      updated_at: '2026-01-01T00:00:00Z',
    },
  ])
  const restricted = render(client, 'user')
  expect(restricted).toContain('Example DNS')
  expect(restricted).not.toContain('>Test<')
  const admin = render(client, 'admin')
  expect(admin).toContain('Example DNS')
  expect(admin).toContain('Test')
  client.clear()
})
