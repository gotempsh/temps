// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { EnvironmentConfigurationCard } from './EnvironmentConfigurationCard'

installBrowserWindow()
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

const WARNING =
  'Anti-affinity is on, but 2 replicas can only spread over 1 eligible node(s)'

function renderCard(placementWarning: string | null, nodes: unknown[]) {
  const nodesCalled = { count: 0 }
  mockApi({
    'GET /internal/nodes': () => {
      nodesCalled.count += 1
      return jsonResponse({ nodes })
    },
  })
  const project = { id: 3, slug: 'app' } as unknown as ProjectResponse
  const environment = {
    id: 7,
    project_id: 3,
    name: 'production',
    placement_warning: placementWarning,
    deployment_config: { replicas: 2, antiAffinity: true },
  } as unknown as EnvironmentResponse
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <EnvironmentConfigurationCard
          project={project}
          environment={environment}
          onUpdate={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, nodesCalled }
}

test('the placement warning shows on an install with no active worker', async () => {
  const { view, nodesCalled } = renderCard(WARNING, [])
  await waitFor(() => expect(nodesCalled.count).toBeGreaterThan(0))
  expect(view.getByText(WARNING)).toBeTruthy()
  // The section it used to live in is hidden here.
  expect(view.queryByText('Anti-affinity')).toBeNull()
})

test('nothing is shown when placement has no problem', async () => {
  const { view, nodesCalled } = renderCard(null, [])
  await waitFor(() => expect(nodesCalled.count).toBeGreaterThan(0))
  expect(view.queryByText(/Replicas can.t spread/)).toBeNull()
})

test('submitting without blur saves the edited preferred node order', async () => {
  let submitted:
    { load_balancing?: { preferredNodeIds?: number[] } } | undefined
  const environment = {
    id: 1,
    name: 'production',
    slug: 'production',
    branch: 'main',
    deployment_config: {
      loadBalancing: { strategy: 'preferred_nodes', preferredNodeIds: [0] },
    },
  } as EnvironmentResponse
  const api = mockApi({
    'GET /internal/nodes': () => jsonResponse({ nodes: [] }),
    'PUT /projects/1/environments/1/settings': async (request) => {
      submitted = await request.json()
      return jsonResponse(environment)
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <EnvironmentConfigurationCard
          project={
            { id: 1, slug: 'example', source_type: 'manual' } as ProjectResponse
          }
          environment={environment}
          onUpdate={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const input = view.getByLabelText('Preferred node IDs, in priority order')
  fireEvent.change(input, { target: { value: '3, 5, 0' } })
  // Enter submits the form without blurring the focused input.
  fireEvent.submit(input.closest('form')!)
  await waitFor(() =>
    expect(submitted?.load_balancing?.preferredNodeIds).toEqual([3, 5, 0])
  )
  api.restore()
})

test('clearing a health number then disabling probes keeps the error reachable', async () => {
  const submissions: Array<{
    load_balancing: {
      healthCheck: { enabled: boolean; intervalSeconds: number }
    }
  }> = []
  const environment = {
    id: 1,
    name: 'production',
    slug: 'production',
    branch: 'main',
    deployment_config: {
      loadBalancing: {
        strategy: 'round_robin',
        healthCheck: {
          enabled: true,
          intervalSeconds: 2,
          timeoutSeconds: 2,
          unhealthyThreshold: 3,
          healthyThreshold: 2,
        },
      },
    },
  } as EnvironmentResponse
  mockApi({
    'GET /internal/nodes': () => jsonResponse({ nodes: [] }),
    'PUT /projects/1/environments/1/settings': async (request) => {
      submissions.push(await request.json())
      return jsonResponse(environment)
    },
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <MemoryRouter>
        <EnvironmentConfigurationCard
          project={
            { id: 1, slug: 'app', source_type: 'manual' } as ProjectResponse
          }
          environment={environment}
          onUpdate={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  fireEvent.change(view.getByLabelText('Interval (seconds)'), {
    target: { value: '' },
  })
  fireEvent.click(view.getByLabelText('Active health checks'))
  fireEvent.submit(
    view.getByRole('button', { name: 'Save changes' }).closest('form')!
  )
  await waitFor(() =>
    expect(view.getByText('Enter a whole number from 1 to 3600')).toBeTruthy()
  )
  expect(submissions).toHaveLength(0)
  const interval = view.getByLabelText('Interval (seconds)')
  expect(interval.getAttribute('aria-invalid')).toBe('true')
  fireEvent.change(interval, { target: { value: '5' } })
  fireEvent.submit(interval.closest('form')!)
  await waitFor(() => expect(submissions).toHaveLength(1))
  expect(submissions[0].load_balancing.healthCheck).toMatchObject({
    enabled: false,
    intervalSeconds: 5,
  })
})
