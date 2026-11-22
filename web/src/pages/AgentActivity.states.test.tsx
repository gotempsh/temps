// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { ProjectsContext } from '@/contexts/ProjectsContext-shared'
import type { ProjectsContextType } from '@/contexts/ProjectsContext-shared'
import { AgentActivity } from './AiGateway'

function render(projects: ProjectsContextType) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/ai-gateway/activity']}>
        <ProjectsContext.Provider value={projects}>
          <AgentActivity />
        </ProjectsContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

test('an installation with no services offers to deploy one', () => {
  const html = render({ projects: [], isLoading: false })
  expect(html).toContain('No services yet')
  expect(html).toContain('href="/projects/new"')
  expect(html).not.toContain('No service selected')
})

test('services still loading are not reported as none', () => {
  const html = render({ projects: [], isLoading: true })
  expect(html).not.toContain('No services yet')
})

for (const error of [
  { title: 'Forbidden', status: 403 },
  { title: 'Internal Server Error', status: 500 },
  new TypeError('Failed to fetch'),
]) {
  test(`a failed services read is not an empty installation: ${String(error)}`, () => {
    const html = render({ projects: [], isLoading: false, error })
    expect(html).not.toContain('No services yet')
    expect(html).toContain('data-read-failure')
  })
}
