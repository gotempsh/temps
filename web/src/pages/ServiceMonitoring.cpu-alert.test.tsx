// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceMonitoring } from './ServiceMonitoring'

installBrowserWindow()
afterEach(cleanup)

test('a new alert rule defaults to CPU as a share of the limit, not cores × 100', async () => {
  mockApi({
    'GET /external-services/3': () =>
      jsonResponse({
        service: { id: 3, name: 'db', service_type: 'postgres' },
        current_parameters: {},
      }),
    'GET /external-services/3/metrics/latest': () =>
      jsonResponse({
        'container.cpu_percent': 68.1,
        'container.cpu_limit_cores': 6,
      }),
    'GET /external-services/3/metrics/status': () =>
      jsonResponse({ last_received_at: null }),
    'GET /external-services/3/metrics': () => jsonResponse([]),
    'GET /external-services/3/metrics/by-database': () =>
      jsonResponse({ databases: [] }),
    'GET /external-services/3/metrics/alert-rules': () => jsonResponse([]),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
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

  fireEvent.click(await screen.findByRole('button', { name: /Add rule/ }))

  const dialog = await screen.findByRole('dialog')
  const [metric] = within(dialog).getAllByRole('combobox')
  expect(metric.textContent).toBe('CPU (% of limit or host)')
})
