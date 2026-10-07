// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  getEnvironmentsQueryKey,
  listContainersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import { ProjectDetailHeader } from './ProjectDetailHeader'

function renderHeader(
  status: string,
  currentId: number | null | undefined,
  {
    activeVisitors,
    containers,
  }: { activeVisitors?: number; containers?: { status: string }[] } = {}
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  if (currentId !== undefined) {
    client.setQueryData(getEnvironmentsQueryKey({ path: { project_id: 1 } }), [
      { id: 1, current_deployment_id: currentId, is_preview: false },
    ])
  }
  if (containers) {
    client.setQueryData(
      listContainersQueryKey({ path: { project_id: 1, environment_id: 1 } }),
      {
        containers: containers.map((container, index) => ({
          container_id: `c${index}`,
          container_name: `web-${index}`,
          image_name: 'web:1',
          created_at: '2026-10-06T10:00:00Z',
          ...container,
        })),
        total: containers.length,
      }
    )
  }
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProjectDetailHeader
          project={
            { id: 1, slug: 'temps-cloud-api', name: 'Temps' } as ProjectResponse
          }
          lastDeployment={
            { id: 3513, status, is_current: false } as DeploymentResponse
          }
          onDeploy={() => {}}
          activeVisitorsCount={
            activeVisitors === undefined
              ? undefined
              : { active_visitors: activeVisitors }
          }
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

for (const status of ['running', 'completed', 'stopped']) {
  test(`current deployment remains Deployed when latest is ${status}`, () => {
    const html = renderHeader(status, 3500)
    expect(html).toContain('>Deployed<')
    expect(html).not.toContain('Not deployed')
  })
}
test('a completed historical build does not imply a current deployment', () => {
  expect(renderHeader('completed', null)).toContain('Not deployed')
})
test('loading environments does not flash Not deployed', () => {
  const html = renderHeader('running', undefined)
  expect(html).toContain('Checking deployment')
  expect(html).not.toContain('Not deployed')
})
for (const status of ['pending', 'queued', 'building', 'running']) {
  test(`a first deployment that is ${status} reads Deploying, not Not deployed`, () => {
    const html = renderHeader(status, null)
    expect(html).toContain('>Deploying<')
    expect(html).toContain('animate-spin')
    expect(html).not.toContain('Not deployed')
  })
}
test('a first deployment that cancelled reads Not deployed', () => {
  const html = renderHeader('cancelled', null)
  expect(html).toContain('Not deployed')
  expect(html).not.toContain('Deploying')
})
for (const currentId of [3500, null]) {
  test(`a failed latest deployment reads Failed (live: ${currentId}) and links to the overview`, () => {
    const html = renderHeader('failed', currentId)
    expect(html).toContain('>Failed<')
    expect(html).toContain('href="/projects/temps-cloud-api/project"')
    expect(html).not.toContain('>Deployed<')
    expect(html).not.toContain('Not deployed')
  })
}
test('a live container that exited reads Degraded', () => {
  const html = renderHeader('completed', 3500, {
    containers: [{ status: 'running' }, { status: 'exited' }],
  })
  expect(html).toContain('>Degraded<')
  expect(html).not.toContain('>Deployed<')
})
test('healthy live containers keep Deployed', () => {
  const html = renderHeader('completed', 3500, {
    containers: [{ status: 'running' }],
  })
  expect(html).toContain('>Deployed<')
})
test('a redeploy keeps the live version Deployed', () => {
  const html = renderHeader('running', 3500)
  expect(html).toContain('>Deployed<')
  expect(html).not.toContain('Deploying')
})
for (const activeVisitors of [0, 3]) {
  test(`the live visitors pill is clickable with ${activeVisitors} active visitors`, () => {
    const html = renderHeader('completed', 3500, { activeVisitors })
    const pill = html.match(/<button[^>]*Open Live visitors[^>]*>/)?.[0]
    expect(pill).toBeDefined()
    expect(pill).not.toContain('disabled')
    expect(pill).toContain('cursor-pointer')
  })
}
