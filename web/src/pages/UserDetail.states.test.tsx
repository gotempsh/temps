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
import {
  listAuditLogsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
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

const DAY_MS = 24 * 60 * 60 * 1000

function auditLogsKey(limit: number, operationType?: string) {
  return listAuditLogsOptions({
    query: {
      limit,
      offset: 0,
      user_id: 42,
      operation_type: operationType,
      from: undefined,
      to: undefined,
    },
  }).queryKey
}

function auditLogs(count: number, operationType: string, ageMs: number) {
  return Array.from({ length: count }, (_, id) => ({
    id,
    operation_type: operationType,
    audit_date: Date.now() - ageMs,
  }))
}

// A user whose newest 100 audit entries are all something other than a login.
function busyUserClient() {
  const client = createClient()
  client.setQueryData(usersKey, [
    {
      user: {
        id: 42,
        name: 'Busy User',
        username: 'busy',
        email: 'busy@example.test',
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
  client.setQueryData(
    auditLogsKey(100),
    auditLogs(100, 'PROJECT_UPDATED', DAY_MS)
  )
  return client
}

function failedLoginsCard(html: string) {
  return html.slice(html.indexOf('Failed logins (30d)'))
}

test('logins older than the newest 100 audit entries still count', () => {
  const client = busyUserClient()
  client.setQueryData(
    auditLogsKey(1, 'LOGIN_SUCCESS'),
    auditLogs(1, 'LOGIN_SUCCESS', 90 * DAY_MS)
  )
  client.setQueryData(auditLogsKey(100, 'LOGIN_FAILURE'), [
    ...auditLogs(3, 'LOGIN_FAILURE', 2 * DAY_MS),
    ...auditLogs(1, 'LOGIN_FAILURE', 40 * DAY_MS),
  ])
  const html = render(client)
  expect(html).not.toContain('Never')
  expect(html).toContain('3 months ago')
  expect(failedLoginsCard(html)).toMatch(
    /^Failed logins \(30d\)<\/span><\/div><div[^>]*>3</
  )
  expect(html).not.toContain('No failed attempts')
  client.clear()
})

test('a user with no logins reads as never and no failed attempts', () => {
  const client = busyUserClient()
  client.setQueryData(auditLogsKey(1, 'LOGIN_SUCCESS'), [])
  client.setQueryData(auditLogsKey(100, 'LOGIN_FAILURE'), [])
  const html = render(client)
  expect(html).toContain('Never')
  expect(html).toContain('No failed attempts')
  client.clear()
})

test('a full batch of failed logins is reported as a lower bound', () => {
  const client = busyUserClient()
  client.setQueryData(auditLogsKey(1, 'LOGIN_SUCCESS'), [])
  client.setQueryData(
    auditLogsKey(100, 'LOGIN_FAILURE'),
    auditLogs(100, 'LOGIN_FAILURE', DAY_MS)
  )
  expect(failedLoginsCard(render(client))).toContain('100+')
  client.clear()
})

test('failed login reads are unknown, not never or none', () => {
  const client = busyUserClient()
  const error = { title: 'Internal Server Error', status: 500 }
  fail(client, auditLogsKey(1, 'LOGIN_SUCCESS'), error)
  fail(client, auditLogsKey(100, 'LOGIN_FAILURE'), error)
  const html = render(client)
  expect(html).not.toContain('Never')
  expect(html).not.toContain('No failed attempts')
  expect(failedLoginsCard(html)).toContain('—')
  client.clear()
})
