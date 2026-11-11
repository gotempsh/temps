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
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MonitoringCard } from './MonitoringCard'

installBrowserWindow()
afterEach(cleanup)

function renderCard(
  latest: Record<string, number>,
  cluster = false,
  charted: string[] = [],
  ranges: Record<string, { time: string; value: number | null }[]> = {}
) {
  mockApi({
    'GET /external-services/3/metrics/latest': () => jsonResponse(latest),
    'GET /external-services/3/metrics/status': () =>
      jsonResponse({ last_received_at: null }),
    'GET /external-services/3/metrics': (request) => {
      const metric = new URL(request.url).searchParams.get('metric') ?? ''
      charted.push(metric)
      return jsonResponse(ranges[metric] ?? [])
    },
    'GET /external-services/3/metrics/alert-rules': () => jsonResponse([]),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <MonitoringCard
          serviceId={3}
          engine="postgres"
          metricsEnabled
          cluster={cluster}
        />
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

test('a new alert rule defaults to CPU as a share of the limit, not cores × 100', async () => {
  renderCard({ 'container.cpu_percent': 68.1, 'container.cpu_limit_cores': 6 })

  fireEvent.click(await screen.findByText('Alert rules'))
  fireEvent.click(await screen.findByRole('button', { name: /Add rule/ }))

  const dialog = await screen.findByRole('dialog')
  const [metric] = within(dialog).getAllByRole('combobox')
  expect(metric.textContent).toBe('CPU (% of limit or host)')
})

// One member's latest usage (0.20 cores) beside the busiest member's own
// reading, limit and share (1.5 of 2 cores, 75%), as the health monitor
// records a cluster.
const CLUSTER_LATEST = {
  'container.cpu_percent': 20,
  'container.cluster_cpu_percent': 150,
  'container.cpu_limit_cores': 2,
  'container.cpu_utilization_percent': 75,
}

test("a cluster's tile reads its busiest member's usage against that member's limit", async () => {
  renderCard(CLUSTER_LATEST, true)

  const label = await screen.findByText('CPU (cores, last scrape)')
  expect(label.nextElementSibling?.textContent).toBe('1.50 / 2 cores')
})

test("a single container's tile reads its own recorded usage", async () => {
  renderCard(CLUSTER_LATEST)

  const label = await screen.findByText('CPU (cores, last scrape)')
  expect(label.nextElementSibling?.textContent).toBe('0.20 / 2 cores')
})

test("selecting a cluster's CPU tile charts its busiest member, not the mean of all members", async () => {
  const charted: string[] = []
  renderCard(CLUSTER_LATEST, true, charted)

  fireEvent.click(await screen.findByText('CPU (cores, last scrape)'))
  await waitFor(() =>
    expect(charted).toContain('container.cluster_cpu_percent')
  )
  expect(await screen.findByText('CPU (cores, busiest member)')).toBeTruthy()
})

test("a cluster's CPU chart shows the members' history before its busiest-member series starts", async () => {
  // Right after an upgrade the busiest-member series has no readings yet,
  // while the members' `container.cpu_percent` covers the whole range.
  const charted: string[] = []
  renderCard(CLUSTER_LATEST, true, charted, {
    'container.cluster_cpu_percent': [
      { time: '2026-01-01T00:00:00Z', value: null },
    ],
    'container.cpu_percent': [{ time: '2026-01-01T00:00:00Z', value: 40 }],
  })

  fireEvent.click(await screen.findByText('CPU (cores, last scrape)'))
  await waitFor(() => {
    expect(charted).toContain('container.cluster_cpu_percent')
    expect(charted).toContain('container.cpu_percent')
  })
  await waitFor(() => expect(screen.queryByText('Loading…')).toBeNull())
  expect(screen.queryByText('No data for this range')).toBeNull()
})

test("selecting a single container's CPU tile charts its own usage", async () => {
  const charted: string[] = []
  renderCard(CLUSTER_LATEST, false, charted)

  fireEvent.click(await screen.findByText('CPU (cores, last scrape)'))
  await waitFor(() => expect(charted).toContain('container.cpu_percent'))
  expect(charted).not.toContain('container.cluster_cpu_percent')
})
