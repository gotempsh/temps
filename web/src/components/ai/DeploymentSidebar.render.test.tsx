// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getDeploymentOptions,
  getLastDeploymentOptions,
  getProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { GoalResponse } from '@/api/client/types.gen'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { DeploymentSidebar } from './DeploymentSidebar'
import type { WatchedDeployment } from './goal-presentation'

const goal = {} as GoalResponse

const watched = (deploymentId: number): WatchedDeployment => ({
  projectId: 3,
  deploymentId,
  goal,
})

function deployment(id: number, status: string, isCurrent: boolean) {
  return {
    id,
    project_id: 3,
    status,
    is_current: isCurrent,
    url: `https://app-${id}.example.test`,
    created_at: 1_760_000_000_000,
    environment_id: 1,
    environment: { id: 1, name: 'production', domains: [] },
  }
}

function render(initialSelected?: string) {
  const client = new QueryClient()
  client.setQueryData(getProjectOptions({ path: { id: 3 } }).queryKey, {
    id: 3,
    name: 'taskboard',
    slug: 'taskboard',
  } as never)
  for (const [id, status, current] of [
    [44, 'running', false],
    [43, 'completed', true],
    [41, 'failed', false],
  ] as const) {
    client.setQueryData(
      getDeploymentOptions({ path: { project_id: 3, deployment_id: id } })
        .queryKey,
      deployment(id, status, current) as never
    )
  }
  return renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <DeploymentSidebar
          deployments={[watched(44), watched(43), watched(41)]}
          initialSelected={initialSelected}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

test('every deployment is one row of a single Deployments tab', () => {
  const html = render()
  expect(html.match(/role="tab"/g)?.length).toBe(1)
  expect(html).toContain('Deployments')
  expect(html).toContain('aria-label="Deployments from this chat"')
  for (const id of [44, 43, 41]) expect(html).toContain(`#${id}`)
})

test('the newest is shown first, and the live one is marked', () => {
  const html = render()
  expect(html.indexOf('#44')).toBeLessThan(html.indexOf('#43'))
  expect(html.match(/>Live</g)?.length).toBe(1)
  // The detail below the list is the newest deployment.
  expect(html).toContain('taskboard · #44</h3>')
})

test('a deployment named in the address opens on that deployment', () => {
  const html = render('3:43')
  expect(html).toContain('taskboard · #43</h3>')
  expect(html).toMatch(
    /aria-pressed="true"[^>]*><span[^>]*>taskboard<span[^>]*> · #43/
  )
})

test('following the latest deployment of a project with none yet waits instead of failing', () => {
  const client = new QueryClient()
  client.setQueryData(getProjectOptions({ path: { id: 3 } }).queryKey, {
    id: 3,
    name: 'taskboard',
    slug: 'taskboard',
  } as never)
  client.setQueryData(getLastDeploymentOptions({ path: { id: 3 } }).queryKey, {
    deployment: null,
  } as never)
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <DeploymentSidebar
          deployments={[{ projectId: 3, deploymentId: null, goal }]}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('This project has no deployment yet.')
  expect(html).not.toContain('could not load this deployment')
})

test('chat working tabs retain every label and wrap without a scrolling strip', () => {
  const client = new QueryClient()
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <DeploymentSidebar
          deployments={[]}
          conversationPublicId="test-conversation"
          initialSelected="made"
          onClose={() => undefined}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html.match(/role="tab"/g)?.length).toBe(6)
  for (const label of [
    'Architecture',
    'Made here',
    'Preview',
    'Files',
    'Changes',
    'Terminal',
  ]) {
    expect(html).toContain(`>${label}</span>`)
  }
  const list = html.match(/<div[^>]*role="tablist"[^>]*>/)?.[0] ?? ''
  expect(list).toContain('flex-wrap')
  expect(list).toContain('h-auto')
  expect(list).not.toContain('overflow-x-auto')
  expect(html).not.toContain('sr-only')
  expect(html).toContain('aria-label="Close working tabs"')
})
