// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceMonitoring } from './ServiceMonitoring'

installBrowserWindow()
afterEach(cleanup)

function rule(id: number, name: string, threshold: number) {
  return {
    id,
    name,
    metric_name: 'pg.cache_hit_ratio',
    comparator: '<',
    threshold,
    severity: 'warning',
    enabled: true,
    service_id: 3,
    for_duration_secs: 300,
    created_at: '2026-11-01T10:00:00Z',
    updated_at: '2026-11-01T10:00:00Z',
  }
}

test('a cache hit rule threshold is shown as configured, never clamped', async () => {
  mockApi({
    'GET /external-services/3': () =>
      jsonResponse({
        service: { id: 3, name: 'db', service_type: 'postgres' },
        current_parameters: {},
      }),
    'GET /external-services/3/metrics/latest': () =>
      jsonResponse({ 'pg.connections': 4 }),
    'GET /external-services/3/metrics/status': () =>
      jsonResponse({ last_received_at: null }),
    'GET /external-services/3/metrics': () => jsonResponse([]),
    'GET /external-services/3/metrics/by-database': () =>
      jsonResponse({ databases: [] }),
    'GET /external-services/3/metrics/alert-rules': () =>
      jsonResponse([
        // Entered as 95 meaning 95%: always breached, and must look wrong.
        rule(5, 'Mistyped cache rule', 95),
        rule(6, 'Low cache hit ratio', 0.9),
      ]),
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

  const mistyped = await screen.findByText('Mistyped cache rule')
  expect(mistyped.closest('tr')?.textContent).toContain('< 9500.0%')
  expect(mistyped.closest('tr')?.textContent).not.toContain('100.0%')
  const valid = await screen.findByText('Low cache hit ratio')
  expect(valid.closest('tr')?.textContent).toContain('< 90.0%')
})
