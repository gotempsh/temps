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

test('the cache hit tile says it covers the last 15 min of all databases', async () => {
  mockApi({
    'GET /external-services/3/metrics/latest': () =>
      jsonResponse({ 'pg.cache_hit_ratio': 0.991 }),
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

  const label = await screen.findByText(
    'Cache hit ratio (last 15 min, all databases)'
  )
  expect(label.getAttribute('title')).toContain('weighted by traffic')
  expect(await screen.findByText('99.1%')).toBeTruthy()
})
