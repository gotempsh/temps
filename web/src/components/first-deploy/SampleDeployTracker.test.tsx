// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getLastDeploymentQueryKey,
  getProjectBySlugQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { SampleDeployTracker } from './SampleDeployTracker'

const project = {
  id: 42,
  name: 'hello-temps',
  slug: 'hello-temps',
  source_type: 'manual',
} as unknown as ProjectResponse

function deployment(
  overrides: Partial<DeploymentResponse> = {}
): DeploymentResponse {
  return {
    id: 9,
    project_id: project.id,
    environment_id: 3,
    status: 'running',
    is_current: false,
    created_at: 1_700_000_000_000,
    url: 'hello-temps-1.example.test',
    environment: {
      id: 3,
      name: 'Production',
      slug: 'production',
      domains: ['hello-temps-production.example.test'],
    },
    ...overrides,
  } as DeploymentResponse
}

function render(lastDeployment: DeploymentResponse): string {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  client.setQueryData(
    getProjectBySlugQueryKey({ path: { slug: project.slug } }),
    project
  )
  client.setQueryData(
    getLastDeploymentQueryKey({ path: { id: project.id } }),
    lastDeployment
  )
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SampleDeployTracker projectSlug={project.slug} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

describe('SampleDeployTracker', () => {
  test('shows a stopped deployment with a retry instead of indefinite progress', () => {
    const markup = render(deployment({ status: 'stopped' }))
    expect(markup).toContain('Stopped')
    expect(markup).toContain('Retry deployment')
    expect(markup).not.toContain('Pulling nginxinc/nginx-unprivileged:alpine')
  })
  test('shows progress and a link to the logs while deploying', () => {
    const markup = render(deployment({ status: 'running' }))
    expect(markup).toContain('Deploying')
    expect(markup).toContain('Pulling nginxinc/nginx-unprivileged:alpine')
    expect(markup).toContain('href="/projects/hello-temps/deployments/9"')
    expect(markup).not.toContain('Your first app is live')
  })

  test('shows the live URL and next steps once completed', () => {
    const markup = render(deployment({ status: 'completed', is_current: true }))
    expect(markup).toContain('Your first app is live')
    expect(markup).toContain(
      'href="https://hello-temps-production.example.test"'
    )
    expect(markup).toContain('Add a custom domain')
    expect(markup).toContain('href="/projects/hello-temps/domains"')
    expect(markup).toContain('Add a database')
    expect(markup).toContain('Connect Git')
    expect(markup).toContain('href="/setup"')
  })

  test('explains an image pull failure and offers a retry', () => {
    const markup = render(
      deployment({
        status: 'failed',
        cancelled_reason:
          'Failed to pull image nginx:alpine: toomanyrequests: rate limit',
      })
    )
    expect(markup).toContain('The image could not be downloaded')
    expect(markup).toContain('toomanyrequests')
    expect(markup).toContain('Retry deployment')
    expect(markup).toContain('Open deployment logs')
  })

  test('explains Docker being unreachable', () => {
    const markup = render(
      deployment({
        status: 'failed',
        cancelled_reason:
          'Cannot connect to the Docker daemon at unix:///var/run/docker.sock',
      })
    )
    expect(markup).toContain('Docker is not reachable from Temps')
    expect(markup).toContain('Start Docker')
  })
})
