// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { getNotificationRouteOptions } from '@/api/client/@tanstack/react-query.gen'
import { NotificationRouteForm } from './NotificationRouteForm'

const routeKey = getNotificationRouteOptions({ path: { id: 7 } }).queryKey

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
      <MemoryRouter initialEntries={['/settings/notifications/routes/7']}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/settings/notifications/routes/:id"
              element={<NotificationRouteForm />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('verified 404 shows notification route not found', () => {
  const client = createClient()
  fail(client, routeKey, { title: 'Not Found', status: 404 })
  const html = render(client)
  expect(html).toContain('Notification route not found')
  expect(html).toContain('Back to Routes')
  client.clear()
})

for (const error of [
  {
    title: 'Internal Server Error',
    status: 500,
    detail: 'route lookup failed: connection reset',
  },
  new TypeError('Failed to fetch'),
]) {
  test(`failed route read is not a missing route: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    fail(client, routeKey, error)
    const html = render(client)
    expect(html).toContain('Notification route unavailable')
    expect(html).not.toContain('Notification route not found')
    expect(html).not.toContain('Route configuration')
    expect(html).toContain('Retry')
    expect(html).toContain('Back to Routes')
    if (!(error instanceof TypeError)) {
      expect(html).toContain('route lookup failed: connection reset')
    }
    client.clear()
  })
}

test('forbidden route read shows access denied, not not-found', () => {
  const client = createClient()
  fail(client, routeKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires notifications read permission',
  })
  const html = render(client)
  expect(html).toContain('Notification route: access denied')
  expect(html).toContain('Requires notifications read permission')
  expect(html).not.toContain('Notification route not found')
  expect(html).toContain('Back to Routes')
  client.clear()
})
