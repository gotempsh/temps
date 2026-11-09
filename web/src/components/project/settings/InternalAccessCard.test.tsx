// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { listInternalAccessGrantsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  InternalAccessGrantListResponse,
  InternalAccessGrantResponse,
} from '@/api/client/types.gen'
import { InternalAccessCard } from './InternalAccessCard'
import { GRANTS_PAGE_SIZE } from './internal-access'

const grant: InternalAccessGrantResponse = {
  id: 3,
  target_project_id: 1,
  target_project_name: 'Orders API',
  target_project_slug: 'orders-api',
  target_environment_id: null,
  target_environment_name: null,
  caller_project_id: 2,
  caller_project_name: 'Checkout worker',
  caller_project_slug: 'checkout-worker',
  caller_environment_id: null,
  caller_environment_name: null,
  description: 'The worker places orders',
  created_by: 9,
  created_at: '2026-11-09T10:00:00Z',
}

function page(
  items: InternalAccessGrantResponse[]
): InternalAccessGrantListResponse {
  return { items, total: items.length, page: 1, page_size: GRANTS_PAGE_SIZE }
}

function render({
  environmentId,
  data,
  failed = false,
}: {
  environmentId?: number
  data?: InternalAccessGrantListResponse
  failed?: boolean
}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  const queryKey = listInternalAccessGrantsQueryKey({
    path: { project_id: 1 },
    query: {
      environment_id: environmentId,
      page: 1,
      page_size: GRANTS_PAGE_SIZE,
    },
  })
  if (data) client.setQueryData(queryKey, data)
  if (failed) {
    client
      .getQueryCache()
      .build(client, { queryKey })
      .setState({
        status: 'error',
        error: new Error('Offline'),
        fetchStatus: 'idle',
      })
  }
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <InternalAccessCard
          projectId={1}
          projectName="Orders API"
          projectSlug="orders-api"
          environmentId={environmentId}
          environmentName={environmentId === undefined ? undefined : 'staging'}
          environmentSlug={environmentId === undefined ? undefined : 'staging'}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  client.clear()
  return markup
}

test('shows a skeleton while the grants load', () => {
  expect(render({})).toContain('aria-busy="true"')
})

test('without grants it says who can call and how to allow another project', () => {
  const markup = render({ data: page([]) })
  expect(markup).toContain(
    'Only Orders API&#x27;s own environments can call it.'
  )
  expect(markup).toContain('http://production.orders-api.temps.local')
  expect(markup).toContain('gets a 403')
  expect(markup).toContain('Calling project')
  expect(markup).toContain('Allow it to call Orders API')
  expect(markup).toContain('id="internal-access"')
})

test('a grant is listed with both scopes and can be removed', () => {
  const markup = render({ data: page([grant]) })
  expect(markup).toContain('Checkout worker, all environments')
  expect(markup).toContain('May call: Every environment')
  expect(markup).toContain('The worker places orders')
  expect(markup).toContain('Remove internal access for Checkout worker')
})

test('an environment card shows project-wide grants as inherited, not removable', () => {
  const markup = render({ environmentId: 10, data: page([grant]) })
  expect(markup).toContain('Internal access to this environment')
  expect(markup).toContain('Project-wide')
  expect(markup).not.toContain('Remove internal access for')
  // The target is this environment: no "May call" picker.
  expect(markup).not.toContain('>May call<')
  expect(markup).toContain('Allow it to call Orders API staging')
})

test('a load failure is reported with a retry', () => {
  const markup = render({ failed: true })
  expect(markup).toContain('Could not load internal access')
  expect(markup).toContain('Try again')
})
