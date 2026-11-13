// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  InternalAccessGrantListResponse,
  InternalAccessGrantResponse,
  InternalAccessSandboxGrantListResponse,
  InternalAccessSandboxGrantResponse,
  ListSandboxesResponse,
  PaginatedProjectList,
  ProjectResponse,
  SandboxInner,
  UserResponse,
} from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { InternalAccessCard } from './InternalAccessCard'
import {
  projectGrantsOptions,
  sandboxGrantsOptions,
  userSandboxesOptions,
} from './internal-access-queries'
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

const sandboxGrant: InternalAccessSandboxGrantResponse = {
  id: 41,
  target_project_id: 1,
  target_project_name: 'Orders API',
  target_project_slug: 'orders-api',
  target_environment_id: 10,
  target_environment_name: 'staging',
  sandbox_id: 'sbx_alpha',
  sandbox_name: 'agent scratchpad',
  sandbox_status: 'running',
  description: null,
  created_by: 9,
  created_at: '2026-11-12T10:00:00Z',
}

function project(id: number, name: string): ProjectResponse {
  return { id, name, slug: name.toLowerCase().replace(/ /g, '-') } as never
}

function sandbox(id: string, name: string, status: string): SandboxInner {
  return { id, name, status } as never
}

function grantList<T>(items: T[]) {
  return { items, total: items.length, page: 1, page_size: 100 }
}

function render({
  environmentId,
  grants,
  sandboxGrants = [],
  failed = false,
  role = 'admin',
}: {
  environmentId?: number
  grants?: InternalAccessGrantResponse[]
  sandboxGrants?: InternalAccessSandboxGrantResponse[]
  failed?: boolean
  role?: string
}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  const projects: PaginatedProjectList = {
    page: 1,
    per_page: GRANTS_PAGE_SIZE,
    total: 3,
    projects: [
      project(1, 'Orders API'),
      project(2, 'Checkout worker'),
      project(3, 'Billing'),
    ],
  }
  client.setQueryData(
    getProjectsOptions({ query: { page: 1, per_page: GRANTS_PAGE_SIZE } })
      .queryKey,
    projects
  )
  const sandboxes: ListSandboxesResponse = {
    pagination: { count: 2 },
    sandboxes: [
      sandbox('sbx_beta', 'idle box', 'stopped'),
      sandbox('sbx_alpha', 'agent scratchpad', 'running'),
    ],
  }
  client.setQueryData(userSandboxesOptions().queryKey, sandboxes)
  const projectKey = projectGrantsOptions(1, environmentId).queryKey
  if (grants) {
    client.setQueryData(
      projectKey,
      grantList(grants) as InternalAccessGrantListResponse
    )
  }
  if (failed) {
    client
      .getQueryCache()
      .build(client, { queryKey: projectKey })
      .setState({
        status: 'error',
        error: new Error('Offline'),
        fetchStatus: 'idle',
      })
  }
  client.setQueryData(
    sandboxGrantsOptions(1, environmentId).queryKey,
    grantList(sandboxGrants) as InternalAccessSandboxGrantListResponse
  )
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <AuthContext.Provider
        value={{
          user: { id: 9, role } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <QueryClientProvider client={client}>
          <InternalAccessCard
            projectId={1}
            projectName="Orders API"
            projectSlug="orders-api"
            environmentId={environmentId}
            environmentName={
              environmentId === undefined ? undefined : 'staging'
            }
            environmentSlug={
              environmentId === undefined ? undefined : 'staging'
            }
          />
        </QueryClientProvider>
      </AuthContext.Provider>
    </MemoryRouter>
  )
  client.clear()
  return markup
}

test('project rows wait, reading "Checking…", while their grants load', () => {
  const markup = render({})
  expect(markup).toContain('aria-label="Allow Checkout worker">Checking…<')
  expect(markup).not.toContain('aria-label="Allow Checkout worker">Allow<')
})

test('without grants it lists every other project and sandbox, each one click to allow', () => {
  const markup = render({ grants: [] })
  expect(markup).toContain('id="internal-access"')
  expect(markup).toContain('curl -sS http://production.orders-api.temps.local/')
  expect(markup).toContain('gets a 403')
  expect(markup).toContain('aria-label="Allow Checkout worker"')
  expect(markup).toContain('aria-label="Allow Billing"')
  // The project does not list itself as a caller.
  expect(markup).not.toContain('aria-label="Allow Orders API"')
  expect(markup).toContain('aria-label="Allow agent scratchpad"')
  expect(markup).toContain('aria-label="Allow Checkout worker with limits"')
  // The old forms are gone.
  expect(markup).not.toContain('Calling project')
  expect(markup).not.toContain('Calling sandbox')
})

test('sandboxes list running ones first', () => {
  const markup = render({ grants: [] })
  expect(markup.indexOf('agent scratchpad')).toBeLessThan(
    markup.indexOf('idle box')
  )
  expect(markup).toContain(
    'Existing callers and your sandboxes, running ones first.'
  )
})

test('an allowed caller reads Allowed with its scope, and can be removed', () => {
  const markup = render({ grants: [grant], sandboxGrants: [sandboxGrant] })
  expect(markup).toContain('Allowed')
  expect(markup).toContain('all environments')
  expect(markup).toContain('The worker places orders')
  expect(markup).toContain(
    'aria-label="Remove internal access for Checkout worker"'
  )
  expect(markup).not.toContain('aria-label="Allow Checkout worker"')
  expect(markup).toContain('staging only')
  expect(markup).toContain(
    'aria-label="Remove internal access for agent scratchpad"'
  )
  // Another project is still one click away.
  expect(markup).toContain('aria-label="Allow Billing"')
  // With grants there is no onboarding line.
  expect(markup).not.toContain('gets a 403')
})

test("an environment's card shows project-wide grants as inherited, not removable", () => {
  const markup = render({ environmentId: 10, grants: [grant] })
  expect(markup).toContain('Internal access to this environment')
  expect(markup).toContain('Project-wide')
  expect(markup).toContain('Set on the project&#x27;s card')
  expect(markup).not.toContain(
    'aria-label="Remove internal access for Checkout worker"'
  )
})

test('a failed grants lookup pauses every Allow and offers a retry', () => {
  const markup = render({ failed: true })
  expect(markup).toContain('Could not check which projects are allowed')
  expect(markup).toContain('Allowing is paused')
  expect(markup).toContain('Try again')
  expect(markup).toContain('Paused')
  // No project row offers Allow: one of them may already be allowed.
  expect(markup).toContain('aria-label="Allow Checkout worker">Paused<')
  expect(markup).not.toContain('aria-label="Allow Billing">Allow<')
})

test('a role without projects:write is told why, with no Allow button', () => {
  const markup = render({ grants: [grant], role: 'reader' })
  expect(markup).toContain(
    'Your role (Reader) cannot change internal access; it needs projects:write.'
  )
  expect(markup).toContain('Needs projects:write')
  expect(markup).not.toContain('aria-label="Allow Billing"')
  expect(markup).not.toContain('aria-label="Remove internal access')
})
