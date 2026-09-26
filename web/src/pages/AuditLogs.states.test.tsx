// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AuthProvider } from '@/contexts/AuthContext'
import {
  getCurrentUserOptions,
  listAuditLogsOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AuditLogs } from './AuditLogs'

const from = '2026-01-01T00:00:00.000Z'
const to = '2026-01-02T00:00:00.000Z'
function renderPage({
  failed = false,
  cached = false,
  role = 'admin',
  page = '1',
} = {}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData(getCurrentUserOptions({}).queryKey, {
    id: 1,
    name: 'Admin',
    username: 'admin',
    avatar_url: '',
    mfa_enabled: false,
    role,
  })
  client.setQueryData(
    listUsersOptions({ query: { include_deleted: false } }).queryKey,
    []
  )
  const query = listAuditLogsOptions({
    query: {
      limit: 20,
      offset: 0,
      from,
      to,
      operation_type: undefined,
      user_id: undefined,
    },
  })
  if (!failed || cached)
    client.setQueryData(
      query.queryKey,
      cached
        ? [
            {
              id: 1,
              operation_type: 'LOGIN_SUCCESS',
              audit_date: Date.parse(from),
              user: null,
              ip_address: null,
              data: {},
            },
          ]
        : []
    )
  if (failed)
    client
      .getQueryCache()
      .build(client, { queryKey: query.queryKey })
      .setState({
        status: 'error',
        error: new Error('Offline'),
        fetchStatus: 'idle',
      })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter
        initialEntries={[
          '/audit-logs?range=' +
            encodeURIComponent(`custom:${from}/${to}`) +
            '&page=' +
            page,
        ]}
      >
        <AuthProvider>
          <BreadcrumbProvider>
            <AuditLogs />
          </BreadcrumbProvider>
        </AuthProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

test('an audit request failure is retryable, not empty', () => {
  const markup = renderPage({ failed: true })
  expect(markup).toContain('Could not load audit logs')
  expect(markup).toContain('Retry')
  expect(markup).not.toContain('No audit logs in this selection')
})
test('refresh failure preserves cached audit rows', () => {
  const markup = renderPage({ failed: true, cached: true })
  expect(markup).toContain('Could not refresh audit logs')
  expect(markup).toContain('Logged in successfully')
})
test('successful empty query offers recovery', () => {
  expect(renderPage()).toContain('No audit logs in this selection')
})
test('invalid URL pages normalize to the first page', () => {
  expect(renderPage({ page: '-2' })).toContain(
    'No audit logs in this selection'
  )
})
test('non-admin users cannot render audit content', () => {
  const markup = renderPage({ role: 'viewer', cached: true })
  expect(markup).not.toContain('Logged in successfully')
  expect(markup).not.toContain('Audit log filters')
})
