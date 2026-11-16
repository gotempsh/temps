// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import {
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from 'react-router'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { RedirectWithQuery } from '@/components/routing/RedirectWithQuery'
import Sandboxes from './Sandboxes'

installBrowserWindow()
const clients = new Set<QueryClient>()
afterEach(async () => {
  await act(async () => {
    cleanup()
    for (const client of clients) client.clear()
    clients.clear()
  })
})

function LocationProbe() {
  const location = useLocation()
  const navigate = useNavigate()
  return (
    <>
      <output data-testid="location">
        {location.pathname}
        {location.search}
      </output>
      <button onClick={() => navigate(-1)}>Back in history</button>
    </>
  )
}

function setup(entry = '/sandboxes?new=sandbox&keep=value', available = true) {
  const api = mockApi({
    'GET /platform/features': () => jsonResponse({ sandboxes: available }),
    'GET /v1/sandboxes': () =>
      jsonResponse({ sandboxes: [], pagination: { next: null, prev: null } }),
    'GET /v1/sandboxes/placement': () =>
      jsonResponse({
        allowed_node_ids: null,
        nodes: [
          {
            id: 0,
            name: 'worker',
            eligible: true,
            allowed: true,
            status: 'active',
            is_control_plane: false,
          },
        ],
      }),
    'POST /v1/sandboxes': () =>
      jsonResponse(
        { routes: [], sandbox: { id: 'sbx_example', name: 'example' } },
        201
      ),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  clients.add(client)
  const view = render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: null,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter initialEntries={[entry]}>
          <Routes>
            <Route
              path="/workspaces"
              element={<RedirectWithQuery to="/sandboxes" />}
            />
            <Route path="/sandboxes" element={<Sandboxes />} />
            <Route
              path="/sandboxes/:id"
              element={<p>Created sandbox detail</p>}
            />
          </Routes>
          <LocationProbe />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
  return { view, api }
}

test('onboarding opens the existing creation form and creation follows the existing detail navigation', async () => {
  const { view, api } = setup()
  await view.findByRole('dialog', { name: 'New sandbox' })
  fireEvent.change(view.getByLabelText('Name'), {
    target: { value: 'example' },
  })
  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))
  await view.findByText('Created sandbox detail')
  expect(view.getByTestId('location').textContent).toBe(
    '/sandboxes/sbx_example'
  )
  expect(api.called('POST', '/v1/sandboxes')).toBe(true)
})

test('closing removes only the creation parameter', async () => {
  const { view } = setup()
  await view.findByRole('dialog', { name: 'New sandbox' })
  fireEvent.click(view.getByRole('button', { name: 'Close' }))
  await waitFor(() =>
    expect(view.getByTestId('location').textContent).toBe(
      '/sandboxes?keep=value'
    )
  )
})

test('the normal New sandbox action uses the same bookmarkable form', async () => {
  const { view } = setup('/sandboxes?keep=value')
  const back = view.getByRole('button', { name: 'Back in history' })
  expect(view.queryByRole('dialog')).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'New sandbox' }))
  await view.findByRole('dialog', { name: 'New sandbox' })
  expect(view.getByTestId('location').textContent).toContain('keep=value')
  expect(view.getByTestId('location').textContent).toContain('new=sandbox')
  fireEvent.click(back)
  await waitFor(() => expect(view.queryByRole('dialog')).toBeNull())
  expect(view.getByTestId('location').textContent).toBe('/sandboxes?keep=value')
})

test('a server without sandbox support explains the missing capability without creating anything', async () => {
  const { view, api } = setup(undefined, false)
  await view.findByText(/Sandboxes are not available on this server/)
  expect(api.called('POST', '/v1/sandboxes')).toBe(false)
})

test('legacy workspace links preserve sandbox creation through the existing redirect', async () => {
  let fixture: ReturnType<typeof setup> | undefined
  await act(async () => {
    fixture = setup('/workspaces?new=sandbox&keep=value')
  })
  if (!fixture) throw new Error('Legacy workspace route did not render')
  const { view, api } = fixture
  await view.findByRole('dialog', { name: 'New sandbox' })
  expect(view.getByTestId('location').textContent).toBe(
    '/sandboxes?new=sandbox&keep=value'
  )
  expect(api.called('POST', '/v1/sandboxes')).toBe(false)
})
