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
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MonitorDetail } from './MonitorDetail'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

test('a failed Refresh keeps the loaded monitor on screen', async () => {
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  let failing = false
  const broken = () =>
    jsonResponse({ title: 'Internal Server Error', status: 500 }, 500)
  mockApi({
    'GET /monitors/5': () =>
      failing
        ? broken()
        : jsonResponse({
            id: 5,
            name: 'Homepage check',
            monitor_type: 'web',
            monitor_url: 'https://app.example.test/',
            check_interval_seconds: 60,
            created_at: '2026-01-01T00:00:00Z',
            is_active: true,
            is_managed: false,
          }),
    'GET /monitors/5/current-status': () =>
      failing
        ? broken()
        : jsonResponse({
            current_status: 'operational',
            uptime_percentage: 99,
          }),
    'GET /monitors/5/bucketed': () =>
      failing ? broken() : jsonResponse({ buckets: [] }),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/monitors/5']}>
        <Routes>
          <Route
            path="/monitors/:monitorId"
            element={
              <MonitorDetail
                project={{ id: 17, slug: 'demo' } as ProjectResponse}
              />
            }
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByRole('heading', { name: 'Homepage check' })
  await new Promise((resolve) => setTimeout(resolve, 5))

  failing = true
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(failure).toHaveBeenCalled())
  await screen.findByText("Couldn't refresh monitor")
  expect(screen.getByRole('heading', { name: 'Homepage check' })).toBeTruthy()
  expect(screen.queryByText('Failed to load monitor')).toBeNull()
})
