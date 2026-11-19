// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type {
  InternalAccessSandboxGrantResponse,
  UserResponse,
} from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SandboxProjectAccess } from './SandboxProjectAccess'

installBrowserWindow()
afterEach(cleanup)

function grant(
  projectId: number,
  sandboxId: string,
  id: number
): InternalAccessSandboxGrantResponse {
  return {
    id,
    target_project_id: projectId,
    target_project_name: `Project ${projectId}`,
    target_project_slug: `project-${projectId}`,
    target_environment_id: null,
    target_environment_name: null,
    sandbox_id: sandboxId,
    sandbox_name: sandboxId,
    sandbox_status: 'running',
    description: null,
    created_by: 9,
    created_at: '2026-11-12T10:00:00Z',
  }
}

function list<T>(items: T[]) {
  return jsonResponse({ items, total: items.length, page: 1, page_size: 100 })
}

const projects = () =>
  jsonResponse({
    projects: [
      { id: 1, name: 'Orders API', slug: 'orders-api' },
      { id: 2, name: 'Storefront', slug: 'storefront' },
    ],
    total: 2,
    page: 1,
    per_page: 20,
  })

function renderAccess(role = 'user') {
  render(
    <MemoryRouter>
      <AuthContext.Provider
        value={{
          user: { id: 9, role } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <QueryClientProvider
          client={
            new QueryClient({ defaultOptions: { queries: { retry: false } } })
          }
        >
          <SandboxProjectAccess sandboxId="sbx_e2e" sandboxName="e2e caller" />
        </QueryClientProvider>
      </AuthContext.Provider>
    </MemoryRouter>
  )
}

test('from the sandbox, each project shows whether it allows this sandbox and allows it in one click', async () => {
  let storefront: InternalAccessSandboxGrantResponse[] = [
    // Another sandbox's grant does not make this one allowed.
    grant(2, 'sbx_other', 50),
  ]
  const bodies: unknown[] = []
  const api = mockApi({
    'GET /projects': projects,
    'GET /projects/1/internal-access/sandboxes': () =>
      list([grant(1, 'sbx_e2e', 40)]),
    'GET /projects/2/internal-access/sandboxes': () => list(storefront),
    'POST /projects/2/internal-access/sandboxes': async (request) => {
      const body = await request.json()
      bodies.push(body)
      storefront = [...storefront, grant(2, 'sbx_e2e', 51)]
      return jsonResponse(storefront[1], 201)
    },
    'DELETE /projects/1/internal-access/sandboxes/40': () =>
      new Response(null, { status: 204 }),
  })
  renderAccess()

  const removeOrders = await screen.findByRole('button', {
    name: 'Remove internal access for Orders API',
  })
  expect(
    within(removeOrders.closest('li') as HTMLElement).getByText('Allowed')
  ).toBeTruthy()

  const allow = screen.getByRole('button', { name: 'Allow Storefront' })
  expect(allow.textContent).toBe('Allow')
  fireEvent.click(allow)
  await waitFor(() => expect(bodies).toEqual([{ sandbox_id: 'sbx_e2e' }]))
  await screen.findByRole('button', {
    name: 'Remove internal access for Storefront',
  })

  fireEvent.click(removeOrders)
  const dialog = await screen.findByRole('alertdialog')
  fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }))
  await waitFor(() =>
    expect(
      api.called('DELETE', '/projects/1/internal-access/sandboxes/40')
    ).toBe(true)
  )
  api.restore()
})

test('a failed lookup for any project on screen pauses every Allow', async () => {
  const api = mockApi({
    'GET /projects': projects,
    'GET /projects/1/internal-access/sandboxes': () => list([]),
    'GET /projects/2/internal-access/sandboxes': () =>
      jsonResponse({ title: 'Unavailable' }, 503),
  })
  renderAccess()

  await screen.findByText('Could not check which projects are allowed')
  for (const name of ['Allow Orders API', 'Allow Storefront']) {
    const button = screen.getByRole('button', { name }) as HTMLButtonElement
    expect(button.textContent).toBe('Paused')
    expect(button.disabled).toBe(true)
  }
  api.restore()
})

test('a role without projects:write sees why each project cannot be allowed', async () => {
  const api = mockApi({
    'GET /projects': projects,
    'GET /projects/1/internal-access/sandboxes': () => list([]),
    'GET /projects/2/internal-access/sandboxes': () => list([]),
  })
  renderAccess('reader')

  await screen.findByText(
    'Your role (Reader) cannot change internal access; it needs projects:write.'
  )
  await waitFor(() =>
    expect(screen.getAllByText('Needs projects:write').length).toBe(2)
  )
  expect(screen.queryByRole('button', { name: 'Allow Storefront' })).toBeNull()
  api.restore()
})
