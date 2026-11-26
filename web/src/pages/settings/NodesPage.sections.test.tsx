// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import {
  AuthContext,
  type AuthContextType,
} from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NodeDetailPage } from './NodesPage'

installBrowserWindow()
afterEach(cleanup)

const node = (role: string) => ({
  id: 2,
  name: 'worker-a',
  role,
  status: 'active',
  address: '10.0.0.2',
  private_address: '10.0.0.2',
  created_at: '2026-01-01T00:00:00Z',
  last_heartbeat: '2026-01-01T00:00:00Z',
  capacity: {},
  labels: {},
  build_limits: {},
  public_ingress_enabled: false,
  public_ingress_unsupported_reasons: [],
})

function renderAt(path: string, role = 'worker') {
  mockApi({ 'GET /internal/nodes/2': () => jsonResponse(node(role)) })
  const auth = {
    user: { role: 'admin' },
    isLoading: false,
    error: null,
    logout: async () => undefined,
    refetch: () => undefined,
  } as unknown as AuthContextType
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AuthContext.Provider value={auth}>
        <MemoryRouter initialEntries={[path]}>
          <BreadcrumbProvider>
            <Routes>
              <Route
                path="/settings/nodes/:nodeId"
                element={<NodeDetailPage />}
              />
              <Route
                path="/settings/nodes/:nodeId/:section"
                element={<NodeDetailPage />}
              />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

const links = (nav: HTMLElement) =>
  [...nav.querySelectorAll('a')].map((a) => [
    a.textContent,
    a.getAttribute('href'),
  ])

test("a server's sections are tabs of its page, under its name", async () => {
  const view = renderAt('/settings/nodes/2')
  const nav = await view.findByRole('navigation', {
    name: 'worker-a sections',
  })
  expect(links(nav)).toEqual([
    ['Overview', '/settings/nodes/2'],
    ['Monitoring', '/settings/nodes/2/monitoring'],
    ['Networking', '/settings/nodes/2/networking'],
    ['Builds', '/settings/nodes/2/builds'],
    ['Scheduling', '/settings/nodes/2/scheduling'],
  ])
  expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Overview'
  )
})

test('the section shown is the tab marked, and a role has only its sections', async () => {
  const view = renderAt('/settings/nodes/2/scheduling', 'edge')
  const nav = await view.findByRole('navigation', {
    name: 'worker-a sections',
  })
  expect(links(nav).map(([title]) => title)).toEqual([
    'Overview',
    'Monitoring',
    'Scheduling',
  ])
  expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Scheduling'
  )
})
