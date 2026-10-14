// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import ProxyMetrics from './ProxyMetrics'

function renderAs(role: string, client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/proxy']}>
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
            <ProxyMetrics />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function nodeMetricQueries(client: QueryClient) {
  return client
    .getQueryCache()
    .getAll()
    .filter((q) => JSON.stringify(q.queryKey).includes('"nodeMetricsGetRange"'))
}

test('a role that cannot read node metrics sees one explanation, not a failure per chart', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const markup = renderAs('user', client)
  expect(markup).toContain('Node metrics: access denied')
  expect(markup.match(/data-read-failure="forbidden"/g)).toHaveLength(1)
  expect(markup).not.toContain('Failed to load proxy metrics')
  expect(nodeMetricQueries(client)).toHaveLength(0)
  client.clear()
})

test('an instance administrator still gets the node metric series', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const markup = renderAs('admin', client)
  expect(markup).not.toContain('Node metrics: access denied')
  expect(nodeMetricQueries(client).length).toBeGreaterThan(0)
  client.clear()
})
