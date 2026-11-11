// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MonitoringCard } from './MonitoringCard'

installBrowserWindow()
afterEach(cleanup)

function renderCard(latest: Record<string, number>) {
  mockApi({
    'GET /external-services/3/metrics/latest': () => jsonResponse(latest),
    'GET /external-services/3/metrics/status': () =>
      jsonResponse({ last_received_at: null }),
    'GET /external-services/3/metrics': () => jsonResponse([]),
    'GET /external-services/3/metrics/alert-rules': () => jsonResponse([]),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <MonitoringCard serviceId={3} engine="postgres" metricsEnabled />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the CPU tile reads in cores against the container limit, like the resources card', async () => {
  // Docker's convention, as stored: 100 == one core. 68.1 is 0.68 cores of
  // a 6-core limit — not "68.1%" of anything the page shows elsewhere.
  renderCard({
    'container.cpu_percent': 68.1,
    'container.cpu_limit_cores': 6,
    'container.cpu_utilization_percent': 11.35,
  })

  const label = await screen.findByText('CPU (cores, last scrape)')
  expect(label.getAttribute('title')).toContain('health check')
  expect(label.nextElementSibling?.textContent).toBe('0.68 / 6 cores')
  expect(screen.queryByText('68.1%')).toBeNull()
})

test('an uncapped container shows cores used without a denominator', async () => {
  renderCard({ 'container.cpu_percent': 85 })

  const label = await screen.findByText('CPU (cores, last scrape)')
  expect(label.nextElementSibling?.textContent).toBe('0.85 cores')
})
