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
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NodesPage } from './NodesPage'

installBrowserWindow()
afterEach(cleanup)

test('Refresh also reloads the mesh and cluster network shown with the nodes', async () => {
  const api = mockApi({
    'GET /internal/nodes': () => jsonResponse([]),
    'GET /nodes/wireguard': () =>
      jsonResponse({ state: 'disabled', nodes: [] }),
    'GET /settings/cluster-network': () => jsonResponse({}),
  })
  const reads = (path: string) =>
    api.calls.filter((call) => call.method === 'GET' && call.path === path)
      .length
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false, staleTime: Infinity } },
        })
      }
    >
      <MemoryRouter>
        <BreadcrumbProvider>
          <NodesPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await waitFor(() => {
    expect(reads('/nodes/wireguard')).toBe(1)
    expect(reads('/settings/cluster-network')).toBe(1)
  })

  fireEvent.click(await screen.findByRole('button', { name: 'Refresh' }))

  await waitFor(() => {
    expect(reads('/nodes/wireguard')).toBe(2)
    expect(reads('/settings/cluster-network')).toBe(2)
  })
})
