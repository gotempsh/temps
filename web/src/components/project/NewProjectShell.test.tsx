// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { NewProjectShell } from './NewProjectShell'

const connectionsKey = listConnectionsOptions().queryKey
const emptyConnections = {
  connections: [],
  page: 1,
  per_page: 20,
  total_count: 0,
}

function createClient() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData(listGitProvidersOptions().queryKey, [])
  return client
}
function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  const query = client.getQueryCache().build(client, { queryKey })
  query.setState({
    ...query.state,
    status: 'error',
    error: error as Error,
    fetchStatus: 'idle',
  })
}
function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <NewProjectShell activeSource="templates" onSelectSource={() => {}}>
          <div />
        </NewProjectShell>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a verified empty connection list says no provider is connected', () => {
  const client = createClient()
  client.setQueryData(connectionsKey, emptyConnections)
  const html = render(client)
  expect(html).toContain('No Git provider connected')
  expect(html).not.toContain('Git connections unavailable')
  client.clear()
})

for (const cached of [false, true]) {
  test(`a failed connections read is not "none connected" (cached empty: ${cached})`, () => {
    const client = createClient()
    if (cached) client.setQueryData(connectionsKey, emptyConnections)
    fail(client, connectionsKey, { title: 'Server error', status: 500 })
    const html = render(client)
    expect(html).toContain('Git connections unavailable')
    expect(html).not.toContain('No Git provider connected')
    client.clear()
  })
}
