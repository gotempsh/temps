// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  listFlagsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectFeatureFlags } from './ProjectFeatureFlags'

const project = {
  id: 1,
  slug: 'sample-app',
  name: 'sample-app',
} as unknown as ProjectResponse
const flagsKey = listFlagsOptions({
  path: { project_id: 1 },
  query: { include_archived: false, page: 1 },
}).queryKey
const environmentsKey = getEnvironmentsOptions({
  path: { project_id: 1 },
}).queryKey
const environments = [
  { id: 7, name: 'production', slug: 'production' },
] as unknown as EnvironmentResponse[]
const emptyPage = {
  flags: [],
  page: 1,
  page_size: 20,
  total: 0,
  total_pages: 1,
}

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
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
        <ProjectFeatureFlags project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'flag store query timed out for project 1',
}
for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed flags read is not an empty project: ${String(error)}`, () => {
    const client = createClient()
    client.setQueryData(environmentsKey, environments)
    fail(client, flagsKey, error)
    const html = render(client)
    expect(html).toContain('Feature flags unavailable')
    expect(html).toContain('Retry')
    expect(html).not.toContain('No feature flags yet')
    expect(html).not.toContain('Integrate your app')
    if (!(error instanceof TypeError)) {
      expect(html).toContain('flag store query timed out for project 1')
    }
    client.clear()
  })
}

test('forbidden flags read shows access denied with the server detail', () => {
  const client = createClient()
  client.setQueryData(environmentsKey, environments)
  fail(client, flagsKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires FeatureFlagsRead permission',
  })
  const html = render(client)
  expect(html).toContain('Feature flags: access denied')
  expect(html).toContain('Requires FeatureFlagsRead permission')
  expect(html).not.toContain('No feature flags yet')
  expect(html).not.toContain('Integrate your app')
  client.clear()
})

test('cached flags stay visible when a refresh fails', () => {
  const client = createClient()
  client.setQueryData(environmentsKey, environments)
  client.setQueryData(flagsKey, {
    ...emptyPage,
    total: 1,
    flags: [
      {
        id: 1,
        key: 'checkout-redesign',
        value_type: 'bool',
        default_value: false,
        client_visible: false,
        environments: [],
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      },
    ],
  })
  fail(client, flagsKey, serverError)
  const html = render(client)
  expect(html).toContain('checkout-redesign')
  expect(html).toContain('Feature flags unavailable')
  expect(html).toContain('Showing last-known data')
  client.clear()
})

test('verified empty list keeps the onboarding empty state', () => {
  const client = createClient()
  client.setQueryData(environmentsKey, environments)
  client.setQueryData(flagsKey, emptyPage)
  const html = render(client)
  expect(html).toContain('No feature flags yet')
  expect(html).toContain('Integrate your app')
  expect(html).not.toContain('Feature flags unavailable')
  client.clear()
})
