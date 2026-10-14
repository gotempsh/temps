// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listTeamsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Teams } from './Teams'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires users:read permission',
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

function signedInAs(role: string): UserResponse {
  return {
    id: 3,
    name: 'Example User',
    username: 'example',
    avatar_url: '',
    mfa_enabled: false,
    role,
  }
}

function render(client: QueryClient, role?: string) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: role ? signedInAs(role) : null,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <Teams />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** The opening tag of the first `<button>` whose content mentions `label`. */
function buttonTag(html: string, label: string): string | undefined {
  const match = new RegExp(
    `<button[^>]*>(?:(?!</button>).)*${label}`,
    's'
  ).exec(html)
  return match ? /<button[^>]*>/.exec(match[0])?.[0] : undefined
}

const teamsKey = listTeamsOptions({
  query: { page: 1, page_size: 100 },
}).queryKey

test('a refused teams read shows the server reason instead of an unreachable API', () => {
  const client = createClient()
  fail(client, teamsKey, refused)
  const html = render(client)
  expect(html).toContain('Teams: access denied')
  expect(html).toContain('Requires users:read permission')
  expect(html).not.toContain('did not respond')
  expect(html).not.toContain('No teams yet')
  client.clear()
})

test('a failed teams read is not an empty team list', () => {
  const client = createClient()
  fail(client, teamsKey, {
    title: 'Internal Server Error',
    status: 500,
    detail: 'teams table unavailable',
  })
  const html = render(client)
  expect(html).toContain('Teams unavailable')
  expect(html).toContain('teams table unavailable')
  expect(html).not.toContain('No teams yet')
  client.clear()
})

const noTeams = { teams: [], total: 0, page: 1, page_size: 100 }

test('a non-admin is offered no Create team action and is told why', () => {
  const client = createClient()
  client.setQueryData(teamsKey, noTeams)
  const html = render(client, 'user')
  expect(buttonTag(html, 'Create team')).toBeUndefined()
  expect(html).toContain('data-admin-only-note')
  expect(html).toContain('Only administrators can create or delete teams')
  expect(html).toContain('Your role is &quot;user&quot;')
  client.clear()
})

test('an administrator keeps the Create team action and sees no restriction note', () => {
  const client = createClient()
  client.setQueryData(teamsKey, noTeams)
  const html = render(client, 'admin')
  expect(buttonTag(html, 'Create team')).toBeDefined()
  expect(html).not.toContain('data-admin-only-note')
  client.clear()
})

test('a refused teams read has no Retry for a role that can never pass it', () => {
  const client = createClient()
  fail(client, teamsKey, refused)
  const html = render(client, 'user')
  expect(html).toContain('Teams: access denied')
  expect(html).not.toContain('Retry')
  client.clear()
})
