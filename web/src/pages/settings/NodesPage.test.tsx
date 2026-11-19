// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { adminListNodesOptions } from '@/api/client/@tanstack/react-query.gen'
import { NodesPage } from './NodesPage'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires settings:read permission',
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
      <MemoryRouter>
        <BreadcrumbProvider>
          <NodesPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const nodesKey = adminListNodesOptions().queryKey

test('a refused node list keeps the page heading and explains the refusal', () => {
  const client = createClient()
  fail(client, nodesKey, refused)
  const html = render(client)
  expect(html).toMatch(/<h[12][^>]*>Worker nodes<\/h[12]>/)
  expect(html).toContain('Worker nodes: access denied')
  expect(html).toContain('Requires settings:read permission')
  expect(html).not.toContain('Failed to load worker nodes')
  // A refused read is refused again on replay, so no Retry is offered.
  expect(html).not.toContain('Retry')
  client.clear()
})
