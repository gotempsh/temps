// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AuthContext } from '@/contexts/AuthContext-shared'
import type { UserResponse } from '@/api/client/types.gen'
import { listUsersOptions } from '@/api/client/@tanstack/react-query.gen'
import { UserDetail } from './UserDetail'

const usersKey = listUsersOptions({ query: { include_deleted: true } }).queryKey
const admin = { id: 1, role: 'admin' } as unknown as UserResponse

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
      <AuthContext.Provider
        value={{
          user: admin,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter initialEntries={['/settings/users/42']}>
          <BreadcrumbProvider>
            <Routes>
              <Route path="/settings/users/:userId" element={<UserDetail />} />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('successful list without the user shows user not found', () => {
  const client = createClient()
  client.setQueryData(usersKey, [
    {
      user: {
        id: 1,
        name: 'Operator',
        username: 'operator',
        email: 'operator@example.test',
        email_verified: true,
        image: '',
        mfa_enabled: false,
        must_change_password: false,
        created_at: Date.parse('2026-01-01T00:00:00Z'),
        updated_at: Date.parse('2026-01-01T00:00:00Z'),
      },
      roles: [],
    },
  ])
  const html = render(client)
  expect(html).toContain('User not found')
  expect(html).not.toContain('User unavailable')
  expect(html).toContain('Back to users')
  client.clear()
})

for (const error of [
  {
    title: 'Internal Server Error',
    status: 500,
    detail: 'user directory query failed',
  },
  new TypeError('Failed to fetch'),
]) {
  test(`failed user list read is not a missing user: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    fail(client, usersKey, error)
    const html = render(client)
    expect(html).toContain('User unavailable')
    expect(html).not.toContain('User not found')
    expect(html).not.toContain('Member since')
    expect(html).not.toContain('Last login')
    expect(html).toContain('Retry')
    expect(html).toContain('Back to users')
    if (!(error instanceof TypeError)) {
      expect(html).toContain('user directory query failed')
    }
    client.clear()
  })
}

test('forbidden user list read shows access denied, not not-found', () => {
  const client = createClient()
  fail(client, usersKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires users read permission',
  })
  const html = render(client)
  expect(html).toContain('User: access denied')
  expect(html).toContain('Requires users read permission')
  expect(html).not.toContain('User not found')
  expect(html).not.toContain('Member since')
  expect(html).toContain('Back to users')
  client.clear()
})
