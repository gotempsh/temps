// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
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
import { ServiceMonitoring } from './ServiceMonitoring'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const REASON = 'Missing permission ExternalServicesWrite'

test('a refused rule delete gives the reason', async () => {
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  mockApi({
    'GET /external-services/3': () =>
      jsonResponse({
        service: { id: 3, name: 'db', service_type: 'postgres' },
        current_parameters: {},
      }),
    'GET /external-services/3/metrics/latest': () =>
      jsonResponse({ postgres_connections_active: 4 }),
    'GET /external-services/3/metrics/status': () =>
      jsonResponse({ last_received_at: null }),
    'GET /external-services/3/metrics': () => jsonResponse([]),
    'GET /external-services/3/metrics/by-database': () => jsonResponse([]),
    'GET /external-services/3/metrics/alert-rules': () =>
      jsonResponse([
        {
          id: 5,
          name: 'High connections',
          metric_name: 'postgres_connections_active',
          comparator: 'gt',
          threshold: 10,
          severity: 'warning',
          enabled: true,
          service_id: 3,
          created_at: '2026-11-01T10:00:00Z',
          updated_at: '2026-11-01T10:00:00Z',
        },
      ]),
    'DELETE /external-services/3/metrics/alert-rules/5': () =>
      jsonResponse({ title: 'Forbidden', detail: REASON }, 403),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/storage/3/monitoring']}>
          <Routes>
            <Route
              path="/storage/:id/monitoring"
              element={<ServiceMonitoring />}
            />
          </Routes>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )

  const trigger = await screen.findByRole('button', {
    name: 'Delete alert rule',
  })
  await act(async () => {
    fireEvent.click(trigger)
  })
  const dialog = await screen.findByRole('dialog')
  const confirm = Array.from(dialog.querySelectorAll('button')).find((b) =>
    b.textContent?.includes('Delete')
  ) as HTMLButtonElement
  await act(async () => {
    fireEvent.click(confirm)
  })

  await waitFor(() =>
    expect(failure).toHaveBeenCalledWith('Failed to delete rule', {
      description: REASON,
    })
  )
})
