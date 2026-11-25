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
import type { ProjectResponse } from '@/api/client'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectStorage } from './ProjectStorage'

installBrowserWindow()
afterEach(cleanup)

const AT = '2026-01-01T00:00:00Z'
const linked = {
  id: 100,
  service: {
    id: 7,
    name: 'orders-db',
    service_type: 'postgres',
    status: 'running',
    created_at: AT,
    updated_at: AT,
  },
}

test("a failed server-wide picker keeps the service's linked databases", async () => {
  let serverCalls = 0
  mockApi({
    'GET /external-services/projects/11': () => jsonResponse([linked]),
    'GET /external-services': () => {
      serverCalls += 1
      return jsonResponse({ title: 'Unavailable' }, 500)
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <BreadcrumbProvider>
          <ProjectStorage
            project={{ id: 11, slug: 'web', name: 'web' } as ProjectResponse}
          />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(await screen.findByText('orders-db')).toBeTruthy()
  expect(serverCalls).toBe(0)

  fireEvent.click(screen.getByRole('button', { name: 'Choose database' }))
  expect(
    await screen.findByText('The databases on this server could not be loaded')
  ).toBeTruthy()
  // The working collection stays, with no page-level failure.
  expect(screen.getByText('orders-db')).toBeTruthy()
  expect(screen.queryByText('Databases could not be loaded')).toBeNull()

  const pickerAlert = screen
    .getByText('The databases on this server could not be loaded')
    .closest('[role="alert"]') as HTMLElement
  fireEvent.click(within(pickerAlert).getByRole('button', { name: 'Retry' }))
  await waitFor(() => expect(serverCalls).toBe(2))
})
