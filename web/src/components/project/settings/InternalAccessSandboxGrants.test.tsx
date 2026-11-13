// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  listInternalAccessSandboxGrantsQueryKey,
  listSandboxesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  InternalAccessSandboxGrantListResponse,
  InternalAccessSandboxGrantResponse,
} from '@/api/client/types.gen'
import { InternalAccessSandboxGrants } from './InternalAccessSandboxGrants'
import { GRANTS_PAGE_SIZE } from './internal-access'

const grant: InternalAccessSandboxGrantResponse = {
  id: 41,
  target_project_id: 1,
  target_project_name: 'Orders API',
  target_project_slug: 'orders-api',
  target_environment_id: null,
  target_environment_name: null,
  sandbox_id: 'sbx_alpha',
  sandbox_name: 'agent scratchpad',
  sandbox_status: 'running',
  description: 'The agent tests checkout',
  created_by: 9,
  created_at: '2026-11-12T10:00:00Z',
}

function render(
  environmentId: number | undefined,
  items?: InternalAccessSandboxGrantResponse[]
) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  if (items) {
    const data: InternalAccessSandboxGrantListResponse = {
      items,
      total: items.length,
      page: 1,
      page_size: GRANTS_PAGE_SIZE,
    }
    client.setQueryData(
      listInternalAccessSandboxGrantsQueryKey({
        path: { project_id: 1 },
        query: {
          environment_id: environmentId,
          page: 1,
          page_size: GRANTS_PAGE_SIZE,
        },
      }),
      data
    )
  }
  client.setQueryData(
    listSandboxesOptions({ query: { page: 1, page_size: 100 } }).queryKey,
    { pagination: { count: 0 }, sandboxes: [] } as never
  )
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <InternalAccessSandboxGrants
          projectId={1}
          projectName="Orders API"
          projectSlug="orders-api"
          environmentId={environmentId}
          environmentName={environmentId === undefined ? undefined : 'staging'}
          environmentSlug={environmentId === undefined ? undefined : 'staging'}
          ownEnvironments={[]}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  client.clear()
  return markup
}

test('without sandbox callers it shows the refused curl and how to add one', () => {
  const markup = render(undefined, [])
  expect(markup).toContain('No sandbox may call it.')
  expect(markup).toContain('curl -sS http://production.orders-api.temps.local/')
  expect(markup).toContain('gets a')
  expect(markup).toContain('Calling sandbox')
  expect(markup).toContain('You have no sandboxes')
  expect(markup).toContain('Allow the sandbox to call Orders API')
  expect(markup).toContain('href="/sandboxes"')
})

test('a sandbox caller is listed and can be removed', () => {
  const markup = render(undefined, [grant])
  expect(markup).toContain('Sandbox agent scratchpad · sbx_alpha')
  expect(markup).toContain('May call: Every environment')
  expect(markup).toContain('The agent tests checkout')
  expect(markup).toContain(
    'Remove internal access for Sandbox agent scratchpad · sbx_alpha'
  )
})

test("on an environment's card a project-wide sandbox grant is inherited", () => {
  const markup = render(10, [grant])
  expect(markup).toContain('Project-wide')
  expect(markup).not.toContain('Remove internal access for')
  expect(markup).not.toContain('Sandbox may call')
  expect(markup).toContain('Allow the sandbox to call Orders API staging')
})

test('a skeleton shows while sandbox callers load', () => {
  expect(render(undefined)).toContain('aria-busy="true"')
})
