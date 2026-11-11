// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MetricTile, ServiceMonitoring } from './ServiceMonitoring'

installBrowserWindow()
afterEach(cleanup)

describe('container CPU tiles on the monitoring dashboard', () => {
  test('the CPU tile shows cores against the limit and says how fresh it is', () => {
    const markup = renderToStaticMarkup(
      <MetricTile
        name="container.cpu_percent"
        value={68.1}
        display="0.68 / 6 cores"
        selected={false}
      />
    )
    expect(markup).toContain('CPU (cores, last scrape)')
    expect(markup).toContain('previous health check')
    expect(markup).toContain('0.68 / 6 cores')
    expect(markup).not.toContain('68.1%')
  })

  test('without a display override the stored value still reads in cores', () => {
    const markup = renderToStaticMarkup(
      <MetricTile name="container.cpu_percent" value={68.1} selected={false} />
    )
    expect(markup).toContain('0.68 cores')
  })

  test('the utilization tile is a percentage of the limit', () => {
    const markup = renderToStaticMarkup(
      <MetricTile
        name="container.cpu_utilization_percent"
        value={11.35}
        selected={false}
      />
    )
    expect(markup).toContain('CPU (% of limit or host)')
    expect(markup).toContain('11.3%')
  })
})

// The latest cpu_percent is one member's (0.20 cores); the cluster reading,
// limit and share are the busiest member's, 1.5 of 2 cores, as the health
// monitor records a cluster. A single container has no cluster reading.
function renderDashboard(topology: 'standalone' | 'cluster') {
  mockApi({
    'GET /external-services/3': () =>
      jsonResponse({
        service: { id: 3, name: 'db', service_type: 'postgres', topology },
        current_parameters: {},
      }),
    'GET /external-services/3/metrics/latest': () =>
      jsonResponse({
        'container.cpu_percent': 20,
        'container.cluster_cpu_percent': 150,
        'container.cpu_limit_cores': 2,
        'container.cpu_utilization_percent': 75,
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
}

describe('the dashboard CPU tile for each topology', () => {
  test("a cluster reads its busiest member's usage against that member's limit", async () => {
    renderDashboard('cluster')
    expect(await screen.findByText('1.50 / 2 cores')).toBeTruthy()
  })

  test('a single container reads its own recorded usage', async () => {
    renderDashboard('standalone')
    expect(await screen.findByText('0.20 / 2 cores')).toBeTruthy()
    expect(screen.queryByText('1.50 / 2 cores')).toBeNull()
  })
})
