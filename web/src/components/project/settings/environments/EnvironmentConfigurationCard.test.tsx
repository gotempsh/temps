// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EnvironmentConfigurationCard } from './EnvironmentConfigurationCard'

installBrowserWindow()
afterEach(cleanup)

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
    'GET /admin/nodes': () => jsonResponse({ nodes: [] }),
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
