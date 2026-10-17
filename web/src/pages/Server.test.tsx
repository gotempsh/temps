// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { Server } from './Server'

function renderAs(role: string, client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/monitoring/server']}>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Example User',
              username: 'example',
              avatar_url: '',
              mfa_enabled: false,
              role,
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <Server />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('server renders its own heading and controls without alert settings or preferences queries', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const markup = renderAs('admin', client)
  expect(markup.match(/<h1/g)).toHaveLength(1)
  expect(markup).toContain('Server</h1>')
  expect(markup).toContain('Pause')
  expect(markup).not.toContain('Monitoring &amp; Alerts')
  expect(markup).not.toContain('role="tablist"')
  expect(
    client.getQueryCache().find({ queryKey: ['preferences'] })
  ).toBeUndefined()
  client.clear()
})

test('a role that cannot read node metrics gets one explanation and no metric queries', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const markup = renderAs('user', client)
  expect(markup).toContain('Node metrics: access denied')
  expect(markup).toContain('settings:read')
  expect(markup).not.toContain('Pause')
  // An idle (disabled) query is registered but never fetches.
  const fetching = client
    .getQueryCache()
    .getAll()
    .filter((q) => q.state.fetchStatus !== 'idle')
  expect(fetching).toHaveLength(0)
  client.clear()
})
