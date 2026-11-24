// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type {
  EnvironmentResponse,
  ProjectResponse,
  UserResponse,
} from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { EnvironmentDetail } from './EnvironmentDetail'

installBrowserWindow()
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

const project = {
  id: 3,
  slug: 'app',
  name: 'app',
} as unknown as ProjectResponse
const environment = {
  id: 7,
  project_id: 3,
  name: 'production',
  slug: 'production',
  main_url: 'https://app-production.example.test',
  subdomain: 'app-production',
  deployment_config: { replicas: 1 },
} as unknown as EnvironmentResponse

/** The workload plane's environment endpoints: internal, operator-only. */
const WORKLOAD_PLANE_PATHS = [
  '/internal/environments/7/workload-mode',
  '/internal/environments/7/workload-isolation',
  '/internal/environments/7/placement/explain',
  '/internal/environments/7/placement/recommendation',
]

test('environment settings neither show nor ask for the workload plane', async () => {
  const api = mockApi({
    'GET /projects/3/environments/7': () => jsonResponse(environment),
    // Still served to operators: answered, so a regression would render.
    'GET /internal/environments/7/workload-mode': () =>
      jsonResponse({ environment_id: 7, workload_mode: 'legacy' }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      {/* An instance admin: the one role the workload plane card spoke to. */}
      <AuthContext.Provider
        value={{
          user: { id: 1, role: 'admin' } as unknown as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter>
          <EnvironmentDetail
            project={project}
            environmentId={environment.id}
            initialEnvironment={environment}
          />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )

  // The cards beside where it used to be have loaded and asked their APIs.
  await waitFor(() => {
    expect(view.getAllByText('Volumes').length).toBeGreaterThan(0)
    expect(
      api.calls.some((call) =>
        call.path.startsWith('/internal/environments/7/volumes')
      )
    ).toBe(true)
  })

  expect(view.queryByText('Workload Plane')).toBeNull()
  for (const mode of ['Shadow', 'Switch to Enforce'])
    expect(view.queryByText(mode)).toBeNull()
  expect(
    api.calls.filter((call) => WORKLOAD_PLANE_PATHS.includes(call.path))
  ).toEqual([])
})
