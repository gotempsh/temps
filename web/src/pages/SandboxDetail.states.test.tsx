// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { getSandboxOptions } from '@/api/client/@tanstack/react-query.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import SandboxDetail from './SandboxDetail'

const sandboxKey = getSandboxOptions({ path: { id: 'sbx-1' } }).queryKey

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
          user: null,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter initialEntries={['/sandboxes/sbx-1']}>
          <Routes>
            <Route path="/sandboxes/:sandboxId" element={<SandboxDetail />} />
          </Routes>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('verified 404 shows sandbox not found', () => {
  const client = createClient()
  fail(client, sandboxKey, { title: 'Not Found', status: 404 })
  const html = render(client)
  expect(html).toContain('Sandbox not found')
  expect(html).toContain('Back to sandboxes')
  client.clear()
})

for (const error of [
  {
    title: 'Internal Server Error',
    status: 500,
    detail: 'sandbox store timed out',
  },
  new TypeError('Failed to fetch'),
]) {
  test(`failed sandbox read is not a missing sandbox: ${error instanceof TypeError ? 'network error' : 'HTTP 500'}`, () => {
    const client = createClient()
    fail(client, sandboxKey, error)
    const html = render(client)
    expect(html).toContain('Sandbox unavailable')
    expect(html).not.toContain('Sandbox not found')
    expect(html).not.toContain('Failed to fetch')
    expect(html).toContain('Retry')
    expect(html).toContain('Back to sandboxes')
    if (!(error instanceof TypeError)) {
      expect(html).toContain('sandbox store timed out')
    }
    client.clear()
  })
}

test('forbidden sandbox read shows access denied, not not-found', () => {
  const client = createClient()
  fail(client, sandboxKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires sandboxes:read permission',
  })
  const html = render(client)
  expect(html).toContain('Sandbox: access denied')
  expect(html).toContain('Requires sandboxes:read permission')
  expect(html).not.toContain('Sandbox not found')
  expect(html).toContain('Back to sandboxes')
  client.clear()
})
