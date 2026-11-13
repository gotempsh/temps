// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { toast } from 'sonner'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceDetail } from './ServiceDetail'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

test('a failed Refresh keeps the loaded service page', async () => {
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  let calls = 0
  mockApi({
    'GET /external-services/3': () =>
      ++calls === 1
        ? jsonResponse({
            sensitive_parameters: [],
            current_parameters: {},
            service: {
              id: 3,
              name: 'orders-db',
              service_type: 'postgres',
              status: 'running',
              created_at: '2026-01-01T00:00:00Z',
              updated_at: '2026-01-01T00:00:00Z',
            },
          })
        : jsonResponse({ title: 'Internal Server Error', status: 500 }, 500),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/storage/3']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/storage/:id" element={<ServiceDetail />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByRole('heading', { name: 'orders-db' })
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(failure).toHaveBeenCalled())
  await screen.findByText("Couldn't refresh service")
  expect(screen.getByRole('heading', { name: 'orders-db' })).toBeTruthy()
  expect(screen.queryByText("Couldn't load service")).toBeNull()
})

test('Refresh also reads the service health shown in the header again', async () => {
  let healthReads = 0
  mockApi({
    'GET /external-services/3': () =>
      jsonResponse({
        sensitive_parameters: [],
        current_parameters: {},
        service: {
          id: 3,
          name: 'orders-db',
          service_type: 'postgres',
          status: 'running',
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:00:00Z',
        },
      }),
    'GET /external-services/3/health-status': () => {
      healthReads++
      // The monitor saw the service go down since the page loaded.
      return jsonResponse({
        service_id: 3,
        status: healthReads === 1 ? 'operational' : 'down',
        consecutive_failures: healthReads === 1 ? 0 : 3,
        recent_checks: [],
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/storage/3']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/storage/:id" element={<ServiceDetail />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByRole('heading', { name: 'orders-db' })
  await waitFor(() => expect(healthReads).toBe(1))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(healthReads).toBe(2))
  await waitFor(() =>
    expect(screen.getAllByText('Down').length).toBeGreaterThan(0)
  )
})
