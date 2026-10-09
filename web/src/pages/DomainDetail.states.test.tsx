// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Routes, Route } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import {
  getDomainByIdOptions,
  getDomainOrderOptions,
  listDnsProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { DomainDetail } from './DomainDetail'

const domainKey = getDomainByIdOptions({ path: { domain: 1 } }).queryKey
const orderKey = getDomainOrderOptions({ path: { domain_id: 1 } }).queryKey
const domain = {
  id: 1,
  domain: 'app.example.test',
  status: 'pending',
  verification_method: 'dns-01',
  is_wildcard: false,
  created_at: Date.parse('2026-01-01T00:00:00Z'),
  updated_at: Date.parse('2026-01-01T00:00:00Z'),
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
      <MemoryRouter initialEntries={['/domains/1']}>
        <PlatformAccessContext.Provider
          value={{
            accessInfo: undefined,
            isLoading: false,
            error: null,
            refetch: () => {},
            isLocal: false,
            isNat: false,
            isCloudflare: false,
            isDirect: true,
          }}
        >
          <BreadcrumbProvider>
            <Routes>
              <Route path="/domains/:id" element={<DomainDetail />} />
            </Routes>
          </BreadcrumbProvider>
        </PlatformAccessContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}
for (const error of [
  { title: 'Forbidden', status: 403 },
  { title: 'Server error', status: 500 },
  new TypeError('Failed to fetch'),
]) {
  test(`failed domain read is not a missing record: ${String(error)}`, () => {
    const client = createClient()
    fail(client, domainKey, error)
    const html = render(client)
    expect(html).toContain('Domain unavailable')
    expect(html).not.toContain('Domain not found')
    expect(html).toContain('Retry')
    expect(html).toContain('Back to Domains')
    expect(html).toContain(
      error instanceof TypeError || error.status === 500
        ? 'Could not contact Temps'
        : 'permission to read'
    )
    client.clear()
  })
}
test('only verified 404 shows not found', () => {
  const client = createClient()
  fail(client, domainKey, { title: 'Not Found', status: 404 })
  expect(render(client)).toContain('Domain not found')
  client.clear()
})
test('cached domain and order survive failed refreshes without offering a new order', async () => {
  const client = createClient()
  client.setQueryData(domainKey, domain)
  client.setQueryData(orderKey, {
    id: 2,
    domain_id: 1,
    identifiers: [],
    order_url: 'https://acme.example.test/order/2',
    status: 'pending',
    email: 'operator@example.test',
    created_at: domain.created_at,
    updated_at: domain.updated_at,
  })
  fail(client, domainKey, new TypeError('Failed to fetch'))
  fail(client, orderKey, { title: 'Forbidden', status: 403 })
  const html = render(client)
  expect(html).toContain('app.example.test')
  expect(html).toContain('Certificate order unavailable')
  expect(html).toContain('Showing last-known data')
  expect(html).toContain('ACME order')
  expect(html).not.toContain('Create new order')
  await client.fetchQuery({
    queryKey: domainKey,
    queryFn: async () => domain,
    staleTime: 0,
  })
  client.removeQueries({ queryKey: orderKey })
  await client
    .fetchQuery({
      queryKey: orderKey,
      queryFn: async () => {
        throw { title: 'Not Found', status: 404 }
      },
      staleTime: 0,
    })
    .catch(() => {})
  const recovered = render(client)
  expect(recovered).not.toContain('unavailable')
  expect(recovered).toContain('Create order')
  client.clear()
})
for (const method of ['dns-01', 'http-01', 'acme']) {
  test(`uncached ${method} order failure does not suggest creating an order`, () => {
    const client = createClient()
    client.setQueryData(domainKey, { ...domain, verification_method: method })
    fail(client, orderKey, new TypeError('Failed to fetch'))
    const html = render(client)
    expect(html).toContain('Certificate order unavailable')
    expect(html).not.toContain('Create order')
    expect(html).not.toContain('Create an ACME order')
    client.clear()
  })
}

for (const status of ['active', 'active_renewal_failed']) {
  for (const method of ['dns-01', 'http-01']) {
    for (const cachedOrder of [false, true]) {
      test(`retained ${cachedOrder ? 'cached' : 'uncached'} ${method} order error does not block renewal after becoming ${status}`, () => {
        const client = createClient()
        const pendingDomain = { ...domain, verification_method: method }
        const orderError = new TypeError('Failed to fetch')
        client.setQueryData(domainKey, pendingDomain)
        if (cachedOrder) {
          client.setQueryData(orderKey, {
            id: 2,
            domain_id: 1,
            status: 'pending',
            authorizations: { challenge_type: method },
            identifiers: [],
            order_url: 'https://acme.example.test/order/2',
            email: 'operator@example.test',
            created_at: domain.created_at,
            updated_at: domain.updated_at,
          })
        }
        fail(client, orderKey, orderError)

        const pending = render(client)
        expect(pending).toContain('Certificate order unavailable')
        expect(pending).not.toContain('Create order')
        expect(pending).not.toContain('Start renewal')
        expect(pending).not.toContain('Renew certificate')

        client.setQueryData(domainKey, { ...pendingDomain, status })
        const serving = render(client)
        expect(client.getQueryState(orderKey)?.error).toBe(orderError)
        expect(serving).not.toContain('Certificate order unavailable')
        expect(serving).toContain('Active TLS certificate')
        const renewLabel =
          method === 'dns-01' ? 'Start renewal' : 'Renew certificate'
        const renewalButton = serving
          .match(/<button\b[^>]*>[\s\S]*?<\/button>/g)
          ?.find((button) => button.includes(renewLabel))
        expect(renewalButton).toBeDefined()
        expect(renewalButton).not.toContain('disabled=')
        if (status === 'active_renewal_failed') {
          expect(serving).toContain('Certificate renewal failed')
        }

        client.setQueryData(domainKey, pendingDomain)
        const pendingAgain = render(client)
        expect(pendingAgain).toContain('Certificate order unavailable')
        expect(pendingAgain).not.toContain('Create order')
        expect(pendingAgain).not.toContain(renewLabel)
        client.clear()
      })
    }
  }
}

function dnsChallengeClient() {
  const client = createClient()
  client.setQueryData(domainKey, domain)
  client.setQueryData(orderKey, {
    id: 2,
    domain_id: 1,
    status: 'pending',
    identifiers: [],
    authorizations: {
      challenge_type: 'dns-01',
      dns_txt_records: [
        { name: '_acme-challenge.app.example.test', value: 'challenge-value' },
      ],
    },
    order_url: 'https://acme.example.test/order/2',
    email: 'operator@example.test',
    created_at: domain.created_at,
    updated_at: domain.updated_at,
  })
  return client
}
const providersKey = listDnsProvidersOptions().queryKey

test('DNS challenge with no provider onboards and preserves the manual steps', () => {
  const client = dnsChallengeClient()
  client.setQueryData(providersKey, [])
  const html = render(client)
  expect(html).toContain('Auto-provision records')
  expect(html).toContain('No DNS provider is configured')
  expect(html).toContain('href="/dns-providers"')
  expect(html).toContain('Add DNS provider')
  expect(html).toContain('challenge-value')
  expect(html).toContain('Verify &amp; finalize')
  client.clear()
})

test('DNS provider loading is distinct from missing configuration', () => {
  const client = dnsChallengeClient()
  const html = render(client)
  expect(html).toContain('Loading DNS providers')
  expect(html).not.toContain('No DNS provider is configured')
  client.clear()
})

test('DNS provider failure offers retry instead of implying no configuration', () => {
  const client = dnsChallengeClient()
  fail(client, providersKey, new TypeError('Failed to fetch'))
  const html = render(client)
  expect(html).toContain('Could not load DNS providers')
  expect(html).toContain('Retry DNS providers')
  expect(html).not.toContain('No DNS provider is configured')
  expect(html).toContain('challenge-value')
  client.clear()
})

test('DNS provider refresh failure retains the configured provider and retry', () => {
  const client = dnsChallengeClient()
  client.setQueryData(providersKey, [
    {
      id: 1,
      name: 'Configured DNS',
      provider_type: 'cloudflare',
      credentials: {},
      flat_hostnames_supported: true,
      is_active: true,
      created_at: '2026-01-01T00:00:00Z',
      updated_at: '2026-01-01T00:00:00Z',
    },
  ])
  fail(client, providersKey, new TypeError('Failed to fetch'))
  const html = render(client)
  expect(html).toContain('Auto-create')
  expect(html).toContain('Retry DNS providers')
  expect(html).not.toContain('No DNS provider is configured')
  client.clear()
})
