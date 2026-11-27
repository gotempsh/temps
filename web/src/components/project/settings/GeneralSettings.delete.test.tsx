// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import type { ProjectResponse, UserResponse } from '@/api/client'
import {
  getProjectsOptions,
  listProjectGroupsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { GeneralSettings } from './GeneralSettings'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = {
  id: 7,
  name: 'Sample App',
  slug: 'sample-app',
  main_branch: 'main',
  source_type: 'git',
  cross_project_trace_sharing: true,
  error_source_context_enabled: false,
  serves_own_temps_api: false,
} as unknown as ProjectResponse

test('deleting a service refreshes the lists the sidebar shows', async () => {
  const api = mockApi({
    'DELETE /projects/7': () => new Response(null, { status: 204 }),
  })
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  })
  // What the sidebar holds: one page of services, and every project with
  // its members. Both still list the service about to be deleted.
  const treeServices = getProjectsOptions({
    query: { per_page: 100, page: 1 },
  }).queryKey
  const allGroups = [
    {
      ...listProjectGroupsQueryKey({ query: { page_size: 100 } })[0],
      allPages: true,
    },
  ]
  client.setQueryData(treeServices, {
    projects: [PROJECT],
    total: 1,
    page: 1,
    per_page: 100,
  })
  client.setQueryData(allGroups, { groups: [], total: 0 })

  const view = render(
    <QueryClientProvider client={client}>
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
          <GeneralSettings project={PROJECT} refetch={() => undefined} />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )

  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Delete service' }))
  })
  await act(async () => {
    // Its label holds a copy button, so it is found by its id.
    const typed = await waitFor(() => {
      const input = document.getElementById('confirm-delete-name')
      if (!input) throw new Error('the confirm field is not shown')
      return input
    })
    fireEvent.change(typed, { target: { value: 'Sample App' } })
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Delete' }))
  })

  await waitFor(() => expect(api.called('DELETE', '/projects/7')).toBe(true))
  await waitFor(() =>
    expect(client.getQueryState(treeServices)?.isInvalidated).toBe(true)
  )
  expect(client.getQueryState(allGroups)?.isInvalidated).toBe(true)
  api.restore()
})
