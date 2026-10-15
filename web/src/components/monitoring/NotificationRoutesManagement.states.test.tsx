// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  listNotificationProvidersOptions,
  listNotificationRoutesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  NotificationProviderResponse,
  NotificationRoutePage,
  UserResponse,
} from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { NotificationRoutesManagement } from './NotificationRoutesManagement'

const routesKey = listNotificationRoutesOptions({
  query: { page_size: 100 },
}).queryKey
const providersKey = listNotificationProvidersOptions({
  query: { page_size: 100 },
}).queryKey

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'Failed to list notification routes: database unavailable',
}

const emptyRoutes: NotificationRoutePage = {
  items: [],
  page: 1,
  page_size: 100,
  total: 0,
}

const provider: NotificationProviderResponse = {
  id: 1,
  name: 'Team email',
  provider_type: 'email',
  config: {},
  enabled: true,
  created_at: 0,
  updated_at: 0,
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
      <MemoryRouter initialEntries={['/settings/notifications?tab=routes']}>
        <AuthContext.Provider
          value={{
            user: signedIn(role),
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <NotificationRoutesManagement />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('no providers: asks for a destination first and links to Add Provider', () => {
  const client = createClient()
  client.setQueryData(routesKey, emptyRoutes)
  client.setQueryData(providersKey, [] as NotificationProviderResponse[])
  const html = render(client)
  expect(html).toContain('Add a destination first')
  expect(html).toContain('Add Provider')
  expect(html).toContain('min-h-60')
  expect(html).not.toContain('min-h-[400px]')
  client.clear()
})

test('providers but no routes: offers Create Route to an administrator', () => {
  const client = createClient()
  client.setQueryData(routesKey, emptyRoutes)
  client.setQueryData(providersKey, [provider])
  const html = render(client)
  expect(html).toContain('No notification routes configured')
  expect(html).toContain('Create Route')
  expect(html).not.toContain('Only administrators can')
  client.clear()
})

test('empty states offer no mutating action to a non-administrator', () => {
  const client = createClient()
  client.setQueryData(routesKey, emptyRoutes)
  client.setQueryData(providersKey, [provider])
  const withProvider = render(client, 'reader')
  expect(withProvider).toContain('No notification routes configured')
  expect(withProvider).not.toContain('Create Route')
  expect(withProvider).toContain('Only administrators can add or change routes')

  client.setQueryData(providersKey, [] as NotificationProviderResponse[])
  const withoutProvider = render(client, 'reader')
  expect(withoutProvider).toContain('Add a destination first')
  expect(withoutProvider).not.toContain('Add Provider')
  client.clear()
})

test('failed route read is not reported as having no routes', () => {
  const client = createClient()
  fail(client, routesKey, serverError)
  client.setQueryData(providersKey, [provider])
  const html = render(client)
  expect(html).toContain('Notification routes unavailable')
  expect(html).toContain('Retry')
  expect(html).not.toContain('No notification routes configured')
  expect(html).not.toContain('Create Route')
  client.clear()
})

test('failed provider read is not reported as having no providers', () => {
  const client = createClient()
  client.setQueryData(routesKey, emptyRoutes)
  fail(client, providersKey, new TypeError('Failed to fetch'))
  const html = render(client)
  expect(html).toContain('Notification routes unavailable')
  expect(html).not.toContain('Add a destination first')
  expect(html).not.toContain('No notification routes configured')
  client.clear()
})
