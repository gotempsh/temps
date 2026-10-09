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
import { GitImportClone } from './GitImportClone'
import { newProjectLandingSource } from './newProjectLanding'

const connectionsKey = listConnectionsOptions().queryKey
const providersKey = listGitProvidersOptions().queryKey

function createClient() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData(providersKey, [])
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
function render(client: QueryClient, path = '/projects/new') {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <GitImportClone />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'git connection store unreachable',
}
for (const path of ['/projects/new', '/projects/new?source=browse']) {
  for (const error of [serverError, new TypeError('Failed to fetch')]) {
    test(`failed connections read at ${path} shows a retryable failure: ${String(error)}`, () => {
      const client = createClient()
      fail(client, connectionsKey, error)
      const html = render(client, path)
      expect(html).toContain('Git connections unavailable')
      expect(html).toContain('data-read-failure="failed"')
      expect(html).toContain('Retry')
      expect(html).not.toContain('No Git provider connected')
      expect(html).not.toContain('Connect provider')
      // Not stuck on the loading skeleton.
      expect(html).not.toContain('h-32 w-full rounded-xl')
      if (!(error instanceof TypeError)) {
        expect(html).toContain('git connection store unreachable')
      }
      client.clear()
    })
  }
}

test('forbidden connections read shows access denied with the server detail', () => {
  const client = createClient()
  fail(client, connectionsKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires GitConnectionsRead permission',
  })
  const html = render(client, '/projects/new?source=browse')
  expect(html).toContain('Git connections: access denied')
  expect(html).toContain('Requires GitConnectionsRead permission')
  expect(html).not.toContain('No Git provider connected')
  client.clear()
})

test('verified empty connection list keeps the connect onboarding', () => {
  const client = createClient()
  client.setQueryData(connectionsKey, {
    connections: [],
    page: 1,
    per_page: 20,
    total_count: 0,
  })
  const html = render(client, '/projects/new?source=browse')
  expect(html).toContain('No Git provider connected')
  expect(html).toContain('Connect provider')
  expect(html).not.toContain('Git connections unavailable')
  client.clear()
})

test('loading connections shows skeletons, not "No Git provider connected"', () => {
  const client = createClient()
  const html = render(client)
  expect(html).toContain('h-32 w-full rounded-xl')
  expect(html).not.toContain('No Git provider connected')
  expect(html).not.toContain('Git connections unavailable')
  client.clear()
})

test('landing source: templates only for a verified empty list', () => {
  expect(newProjectLandingSource(undefined)).toBeNull()
  expect(newProjectLandingSource({ connections: [] })).toBe('templates')
  expect(newProjectLandingSource({ connections: [{ id: 1 }] })).toBe('browse')
})
