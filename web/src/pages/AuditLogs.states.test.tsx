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

import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { mockApi, jsonResponse } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
installBrowserWindow()

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

test('resource scope reaches every audit page and can be explicitly removed', async () => {
  const seen: string[] = []
  const api = mockApi({
    'GET /audit/logs': (request) => {
      const query = new URL(request.url).searchParams
      seen.push(`${query.get('resource_id')}:${query.get('offset')}`)
      return jsonResponse(
        Array.from({ length: 20 }, (_, i) => ({
          id: i + 1,
          operation_type: 'UPDATE_ACCESS_VAULT',
          audit_date: Date.parse(from),
          data: { resource_id: 'vault_fixture' },
        }))
      )
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(getCurrentUserOptions({}).queryKey, {
    id: 1,
    name: 'Admin',
    username: 'admin',
    avatar_url: '',
    mfa_enabled: false,
    role: 'admin',
  })
  client.setQueryData(
    listUsersOptions({ query: { include_deleted: false } }).queryKey,
    []
  )
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={['/audit-logs?range=all&resource=vault_fixture']}
        >
          <AuthProvider>
            <BreadcrumbProvider>
              <AuditLogs />
            </BreadcrumbProvider>
          </AuthProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() => expect(seen).toContain('vault_fixture:0'))
    await waitFor(() =>
      expect(
        (ui.getByRole('button', { name: 'Next' }) as HTMLButtonElement).disabled
      ).toBe(false)
    )
    await act(async () =>
      fireEvent.click(ui.getByRole('button', { name: 'Next' }))
    )
    await waitFor(() => expect(seen).toContain('vault_fixture:20'))
    expect(ui.getByText('Resource: vault_fixture')).toBeTruthy()
    await act(async () =>
      fireEvent.click(ui.getByRole('button', { name: 'Show all resources' }))
    )
    await waitFor(() => expect(seen).toContain('null:0'))
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
