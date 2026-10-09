// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { listUsersOptions } from '@/api/client/@tanstack/react-query.gen'
import type { RouteUserWithRoles } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { Users } from './Users'

const listKey = listUsersOptions({ query: { include_deleted: false } }).queryKey

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'Failed to list users: database connection refused',
}
const forbidden = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires UsersRead permission',
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
      <MemoryRouter initialEntries={['/settings/users']}>
        <AuthContext.Provider
          value={{
            user: null,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <Users />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** The page header always offers "Add User"; the empty state adds a second. */
function addUserButtons(html: string) {
  return html.split('Add User').length - 1
}

function expectNoEmptyState(html: string) {
  expect(html).not.toContain('No users found')
  expect(html).not.toContain('Get started by creating a new user.')
  expect(addUserButtons(html)).toBe(1)
}

for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed user read is not an empty list: ${String(error)}`, () => {
    const client = createClient()
    fail(client, listKey, error)
    const html = render(client)
    expect(html).toContain('Users unavailable')
    expect(html).toContain('Retry')
    expectNoEmptyState(html)
    if (!(error instanceof TypeError)) {
      expect(html).toContain(serverError.detail)
    }
    client.clear()
  })
}

test('forbidden user read says access denied, not "no users"', () => {
  const client = createClient()
  fail(client, listKey, forbidden)
  const html = render(client)
  expect(html).toContain('Users: access denied')
  expect(html).toContain(forbidden.detail)
  expectNoEmptyState(html)
  client.clear()
})

test('verified empty user list keeps the Add User CTA', () => {
  const client = createClient()
  client.setQueryData(listKey, [] as RouteUserWithRoles[])
  const html = render(client)
  expect(html).toContain('No users found')
  expect(addUserButtons(html)).toBe(2)
  expect(html).not.toContain('unavailable')
  expect(html).not.toContain('access denied')
  client.clear()
})

test('cached users stay visible when a refresh fails', () => {
  const client = createClient()
  const cached: RouteUserWithRoles[] = [
    {
      user: {
        id: 2,
        name: 'Operator One',
        username: 'operator1',
        email: 'operator1@example.test',
        email_verified: true,
        image: '',
        mfa_enabled: false,
        must_change_password: false,
        created_at: Date.parse('2026-01-01T00:00:00Z'),
        updated_at: Date.parse('2026-01-01T00:00:00Z'),
      },
      roles: [],
    },
  ]
  client.setQueryData(listKey, cached)
  fail(client, listKey, forbidden)
  const html = render(client)
  expect(html).toContain('Operator One')
  expect(html).toContain('Users: access denied')
  expect(html).toContain('Showing last-known data')
  expect(html).not.toContain('No users found')
  client.clear()
})
