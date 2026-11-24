// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { listIpAccessControlOptions } from '@/api/client/@tanstack/react-query.gen'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { IpAccessControl } from './IpAccessControl'

const key = listIpAccessControlOptions().queryKey

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function render(client: QueryClient, role?: string) {
  const page = (
    <QueryClientProvider client={client}>
      <IpAccessControl />
    </QueryClientProvider>
  )
  if (!role) return renderToStaticMarkup(page)
  return renderToStaticMarkup(
    <AuthContext.Provider
      value={{
        user: { id: 1, role } as UserResponse,
        isLoading: false,
        error: null,
        logout: async () => {},
        refetch: () => {},
      }}
    >
      {page}
    </AuthContext.Provider>
  )
}

function fail(client: QueryClient, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey: key })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

test('a refused rule read is access denied, not "no blocked IPs"', () => {
  const client = createClient()
  fail(client, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires load_balancer:read permission',
  })
  const html = render(client)
  expect(html).toContain('IP access control rules: access denied')
  expect(html).toContain('Requires load_balancer:read permission')
  expect(html).not.toContain('No blocked IPs configured')
  expect(html).not.toContain('Edit rule')
  expect(html).not.toContain('Delete rule')
  // The write entry point goes away with the read that backs it.
  expect(html).not.toContain('Add IP rule')
  client.clear()
})

test('a verified-empty rule list still shows the empty sections', () => {
  const client = createClient()
  client.setQueryData(key, [])
  const html = render(client)
  expect(html).toContain('No blocked IPs configured')
  expect(html).not.toContain('access denied')
  client.clear()
})

test('a failed refresh keeps the last-known rules beside the failure', () => {
  const client = createClient()
  client.setQueryData(key, [
    {
      id: 1,
      ip_address: '192.0.2.10',
      action: 'block',
      reason: null,
      created_at: '2026-01-01T00:00:00Z',
      updated_at: '2026-01-01T00:00:00Z',
    },
  ])
  fail(client, { title: 'Service Unavailable', status: 503, detail: 'down' })
  const html = render(client, 'admin')
  expect(html).toContain('192.0.2.10')
  expect(html).toContain('IP access control rules unavailable')
  expect(html).toContain('Showing last-known data')
  expect(html).toContain('Add IP rule')
  client.clear()
})

const RULE = {
  id: 1,
  ip_address: '192.0.2.10',
  action: 'block',
  reason: null,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

test('a plain user sees the block list but no way to change it', () => {
  const client = createClient()
  client.setQueryData(key, [RULE])
  const html = render(client, 'user')
  expect(html).toContain('192.0.2.10')
  expect(html).toContain('Only administrators can add, edit or remove IP rules')
  expect(html).toContain('Your role is &quot;user&quot;')
  expect(html).not.toContain('Add IP rule')
  expect(html).not.toContain('Edit rule')
  expect(html).not.toContain('Delete rule')
  client.clear()
})

test('an administrator can add, edit and delete rules', () => {
  const client = createClient()
  client.setQueryData(key, [RULE])
  const html = render(client, 'admin')
  expect(html).toContain('Add IP rule')
  expect(html).toContain('Edit rule')
  expect(html).toContain('Delete rule')
  expect(html).not.toContain('Only administrators can')
  client.clear()
})
