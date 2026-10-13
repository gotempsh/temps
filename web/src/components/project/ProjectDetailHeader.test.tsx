// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import { getEnvironmentsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import { ProjectDetailHeader } from './ProjectDetailHeader'

function renderHeader(
  currentId: number | null | undefined,
  hosting: ProjectResponse['hosting'] = 'hosted'
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  if (currentId !== undefined) {
    client.setQueryData(getEnvironmentsQueryKey({ path: { project_id: 1 } }), [
      { id: 1, current_deployment_id: currentId },
    ])
  }
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProjectDetailHeader
          project={
            {
              id: 1,
              slug: 'temps-cloud-api',
              name: 'Temps',
              hosting,
            } as ProjectResponse
          }
          onDeploy={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

test('an environment with a current deployment reads Deployed', () => {
  const html = renderHeader(3500)
  expect(html).toContain('>Deployed<')
  expect(html).not.toContain('Not deployed')
})
test('a completed historical build does not imply a current deployment', () => {
  expect(renderHeader(null)).toContain('Not deployed')
})
test('loading environments does not flash Not deployed', () => {
  const html = renderHeader(undefined)
  expect(html).toContain('Checking deployment')
  expect(html).not.toContain('Not deployed')
})
test('a service hosted elsewhere shows External and no deploy button', () => {
  const html = renderHeader(null, 'external')
  expect(html).toContain('External')
  expect(html).not.toContain('Not deployed')
  expect(html).not.toContain('>Deploy<')
})
