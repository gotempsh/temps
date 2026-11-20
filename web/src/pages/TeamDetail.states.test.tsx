// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Routes, Route } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { getTeamOptions } from '@/api/client/@tanstack/react-query.gen'
import { TeamDetail } from './TeamDetail'

const teamKey = getTeamOptions({ path: { team_id: 987654 } }).queryKey

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
      <MemoryRouter initialEntries={['/teams/987654']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/teams/:teamId" element={<TeamDetail />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an unknown team says so and links back to the list', () => {
  const client = createClient()
  fail(client, teamKey, { title: 'Resource Not Found', status: 404 })
  const html = render(client)
  expect(html).toContain('Team not found')
  expect(html).toContain('987654')
  expect(html).toContain('href="/settings/teams"')
  // The empty shell of a team that does not exist must not be drawn.
  expect(html).not.toContain('Edit team')
  expect(html).not.toContain('Add member')
  client.clear()
})

for (const error of [
  { title: 'Server error', status: 500 },
  { title: 'Forbidden', status: 403 },
  new TypeError('Failed to fetch'),
]) {
  test(`a failed team read is not reported as a missing team: ${String(error)}`, () => {
    const client = createClient()
    fail(client, teamKey, error)
    const html = render(client)
    expect(html).not.toContain('Team not found')
    expect(html).toContain('data-read-failure')
    expect(html).toContain('href="/settings/teams"')
    client.clear()
  })
}
