// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { listNotificationProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  NotificationProviderResponse,
  UserResponse,
} from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { ProvidersManagement } from './ProvidersManagement'

const listKey = listNotificationProvidersOptions().queryKey

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'Failed to list notification providers: database unavailable',
}
const forbidden = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires NotificationProvidersRead permission',
}

function signedIn(role: string): UserResponse {
  return {
    id: 1,
    name: 'Operator',
    username: 'operator',
    avatar_url: '',
    mfa_enabled: false,
    role,
  }
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

function render(client: QueryClient, role = 'admin') {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/notifications']}>
        <AuthContext.Provider
          value={{
            user: signedIn(role),
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <ProvidersManagement />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('verified empty provider list explains the page and offers Add Provider', () => {
  const client = createClient()
  client.setQueryData(listKey, [] as NotificationProviderResponse[])
  const html = render(client)
  expect(html).toContain('No notification providers configured')
  expect(html).toContain('Add your first notification provider')
  expect(html).toContain('Add provider')
  expect(html).not.toContain('Only administrators can')
  // The compact variant: no 400px legacy panel.
  expect(html).toContain('min-h-60')
  expect(html).not.toContain('min-h-[400px]')
  client.clear()
})

test('empty provider list offers no Add Provider to a non-administrator', () => {
  const client = createClient()
  client.setQueryData(listKey, [] as NotificationProviderResponse[])
  const html = render(client, 'reader')
  expect(html).toContain('No notification providers configured')
  expect(html).not.toContain('Add provider')
  expect(html).toContain('Only administrators can add or change providers')
  client.clear()
})

for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed provider read is not an empty list: ${String(error)}`, () => {
    const client = createClient()
    fail(client, listKey, error)
    const html = render(client)
    expect(html).toContain('Notification providers unavailable')
    expect(html).toContain('Retry')
    expect(html).not.toContain('No notification providers configured')
    expect(html).not.toContain('Add provider')
    client.clear()
  })
}

test('forbidden provider read says access denied and offers no retry', () => {
  const client = createClient()
  fail(client, listKey, forbidden)
  const html = render(client, 'reader')
  expect(html).toContain('Notification providers: access denied')
  expect(html).toContain(forbidden.detail)
  expect(html).not.toContain('No notification providers configured')
  expect(html).not.toContain('>Retry<')
  client.clear()
})
