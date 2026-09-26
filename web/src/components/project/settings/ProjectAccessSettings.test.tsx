// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  listProjectAccessOptions,
  listTeamsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, ProjectAccessResponse } from '@/api/client'
import { ProjectAccessSettings } from './ProjectAccessSettings'

const grant: ProjectAccessResponse = {
  id: 1,
  project_id: 2,
  team_id: 3,
  role: 'viewer',
  granted_by: 1,
  created_at: '2026-01-01',
  updated_at: '2026-01-01',
}
function renderPanel({
  grants = [grant],
  accessFailed = false,
  teamsFailed = false,
  noTeams = false,
  pending = false,
}: {
  grants?: ProjectAccessResponse[]
  accessFailed?: boolean
  teamsFailed?: boolean
  noTeams?: boolean
  pending?: boolean
} = {}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  const access = listProjectAccessOptions({ path: { project_id: 2 } })
  const teams = listTeamsOptions({ query: { page: 1, page_size: 100 } })
  if (!pending && (!accessFailed || grants.length))
    client.setQueryData(access.queryKey, grants)
  if (!pending && !teamsFailed)
    client.setQueryData(teams.queryKey, {
      teams: noTeams
        ? []
        : [
            {
              id: 3,
              name: 'Operations',
              slug: 'operations',
              created_by: 1,
              created_at: '2026-01-01',
              updated_at: '2026-01-01',
            },
          ],
      total: noTeams ? 0 : 1,
      page: 1,
      page_size: 100,
    })
  for (const [failed, queryKey] of [
    [accessFailed, access.queryKey],
    [teamsFailed, teams.queryKey],
  ] as const) {
    if (failed)
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
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProjectAccessSettings project={{ id: 2 } as ProjectResponse} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

test('renders team links and unchanged roles in the shared table', () => {
  const markup = renderPanel()
  expect(markup).toContain('aria-label="Teams with project access"')
  expect(markup).toContain('href="/settings/teams/3"')
  expect(markup).toContain('viewer')
  expect(markup).toContain('Revoking the last grant makes it open again')
})
test('failed team lookup preserves grants instead of claiming there are no teams', () => {
  const markup = renderPanel({ teamsFailed: true })
  expect(markup).toContain('Could not load teams')
  expect(markup).toContain('Team 3')
  expect(markup).not.toContain('No teams exist yet')
})
test('access failures are retryable and never claim the project is open', () => {
  const markup = renderPanel({ accessFailed: true, grants: [] })
  expect(markup).toContain('Could not load access grants')
  expect(markup).not.toContain('Open to everyone')
  expect(markup).not.toContain('No team restrictions')
})
test('cached grants survive refresh failures', () => {
  const markup = renderPanel({ accessFailed: true })
  expect(markup).toContain('Could not refresh access grants')
  expect(markup).toContain('Operations')
})
test('loading retains column headings without asserting open access', () => {
  const markup = renderPanel({ pending: true })
  expect(markup).toContain('Role on this project')
  expect(markup).toContain('aria-busy="true"')
  expect(markup).not.toContain('Open to everyone')
})
test('empty teams onboard while existing teams without grants explain unrestricted access', () => {
  expect(renderPanel({ noTeams: true, grants: [] })).toContain(
    'Create a team to restrict access'
  )
  expect(renderPanel({ grants: [] })).toContain('No team restrictions')
})
