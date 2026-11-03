// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, waitFor } from '@testing-library/react'
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
