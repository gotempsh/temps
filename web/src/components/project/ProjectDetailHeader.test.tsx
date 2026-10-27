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
    hosting = 'hosted',
    preset = 'dockerfile',
    linkEnvironmentId = 1,
    linkContainers,
    linkUrl = 'https://app.example.com',
    unavailableReason,
  }: {
    activeVisitors?: number
    containers?: {
      status: string
      container_port?: number | null
      health_state?: string
    }[]
    hosting?: ProjectResponse['hosting']
    preset?: string
    linkEnvironmentId?: number | null
    linkContainers?: {
      status: string
      container_port?: number | null
      health_state?: string
    }[]
    linkUrl?: string | null
    unavailableReason?: string
  } = {}
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  if (currentId !== undefined) {
    client.setQueryData(getEnvironmentsQueryKey({ path: { project_id: 1 } }), [
      { id: 1, current_deployment_id: currentId, is_preview: false },
      ...(linkEnvironmentId && linkEnvironmentId !== 1
        ? [
            {
              id: linkEnvironmentId,
              current_deployment_id: 3600,
              is_preview: false,
            },
          ]
        : []),
    ])
  }
  if (linkContainers && linkEnvironmentId) {
    client.setQueryData(
      listContainersQueryKey({
        path: { project_id: 1, environment_id: linkEnvironmentId },
      }),
      { containers: linkContainers, total: linkContainers.length }
    )
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
            {
              id: 1,
              slug: 'temps-cloud-api',
              name: 'Temps',
              hosting,
              project_type: 'server',
              preset,
            } as ProjectResponse
          }
          lastDeployment={
            {
              id: 3513,
              status,
              is_current: false,
              environment_id: linkEnvironmentId,
            } as DeploymentResponse
          }
          onDeploy={() => {}}
          lastDeploymentUrl={linkUrl}
          lastDeploymentUnavailableReason={unavailableReason}
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
test('a service hosted elsewhere shows External and no deploy button', () => {
  const html = renderHeader('completed', null, { hosting: 'external' })
  expect(html).toContain('External')
  expect(html).not.toContain('Not deployed')
  expect(html).not.toContain('>Deploy<')
})

for (const health_state of ['healthy', 'unhealthy', 'unknown']) {
  test(`a confirmed worker shows ${health_state} at deploy instead of HTTP controls`, () => {
    const html = renderHeader('completed', 3500, {
      activeVisitors: 0,
      containers: [{ status: 'running', container_port: null, health_state }],
    })
    expect(html).toContain('Background worker')
    expect(html).toContain(`${health_state} at deploy`)
    expect(html).not.toContain('Visit deployed site')
    expect(html).not.toContain('Open Live visitors')
    expect(html).not.toContain('href="/projects/temps-cloud-api/monitors"')
  })
}

for (const container_port of [8080, undefined]) {
  test(`HTTP controls remain available for a web or unclassified container (${container_port})`, () => {
    const html = renderHeader('completed', 3500, {
      activeVisitors: 0,
      containers: [
        { status: 'running', container_port, health_state: 'healthy' },
      ],
    })
    expect(html).not.toContain('Background worker')
    expect(html).toContain('Visit deployed site')
    expect(html).toContain('Open Live visitors')
  })
}

test('Compose header presentation is unchanged for an internal service without a port', () => {
  const html = renderHeader('completed', 3500, {
    preset: 'docker-compose',
    containers: [
      { status: 'running', container_port: null, health_state: 'healthy' },
    ],
  })
  expect(html).not.toContain('Background worker')
  expect(html).toContain('Visit deployed site')
})

test('a production worker does not hide a staging web site link', () => {
  const html = renderHeader('completed', 3500, {
    containers: [
      { status: 'running', container_port: null, health_state: 'healthy' },
    ],
    linkEnvironmentId: 2,
    linkContainers: [
      { status: 'running', container_port: 8080, health_state: 'healthy' },
    ],
  })
  expect(html).toContain('Background worker')
  expect(html).toContain('Visit deployed site')
  expect(html).toContain('href="https://app.example.com"')
})

test('a staging worker link is hidden even when production is a web service', () => {
  const html = renderHeader('completed', 3500, {
    containers: [
      { status: 'running', container_port: 8080, health_state: 'healthy' },
    ],
    linkEnvironmentId: 2,
    linkContainers: [
      { status: 'running', container_port: null, health_state: 'healthy' },
    ],
  })
  expect(html).not.toContain('Visit deployed site')
})

test('unknown staging workload metadata does not suppress its site link', () => {
  const html = renderHeader('completed', 3500, {
    containers: [
      { status: 'running', container_port: null, health_state: 'healthy' },
    ],
    linkEnvironmentId: 2,
    linkContainers: [{ status: 'running' }],
  })
  expect(html).toContain('Visit deployed site')
})

test('a missing link environment does not borrow production worker classification', () => {
  const html = renderHeader('completed', 3500, {
    containers: [
      { status: 'running', container_port: null, health_state: 'healthy' },
    ],
    linkEnvironmentId: null,
  })
  expect(html).toContain('Visit deployed site')
})

test('the visit link follows the primary address it is given', () => {
  const html = renderHeader('completed', 3513, {
    linkUrl: 'https://shop.example.com',
  })
  expect(html).toContain('href="https://shop.example.com"')
})

test('with no address the visit button stays, disabled, and says why', () => {
  const html = renderHeader('completed', 3513, {
    linkUrl: null,
    unavailableReason: 'app-production.localho.st only resolves on this machine',
  })
  expect(html).not.toContain('href="https://')
  expect(html).toContain(
    'title="Nothing to visit yet: app-production.localho.st only resolves on this machine"'
  )
  expect(html).toMatch(/<button[^>]*disabled=""[^>]*aria-label="Visit deployed site: unavailable/)
})
