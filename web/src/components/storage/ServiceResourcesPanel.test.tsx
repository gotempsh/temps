// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceResourcesPanel } from './ServiceResourcesPanel'

installBrowserWindow()
afterEach(cleanup)

type StatsMode = 'ok' | 'member-missing' | 'fails'

function renderPanel(
  nanoCpus: number | null,
  cpuPercent: number,
  stats: StatsMode = 'ok'
) {
  const member = {
    role: 'primary',
    container_name: 'service-postgres',
    cpu_percent: cpuPercent,
    memory_usage_bytes: 8_870_000_000,
    memory_limit_bytes: 34_359_738_368,
    memory_percent: 25.8,
    online_cpus: 16,
  }
  mockApi({
    'GET /external-services/5/runtime': () =>
      jsonResponse({
        service_id: 5,
        topology: 'standalone',
        members: [
          {
            role: 'primary',
            container_name: 'service-postgres',
            container_id: 'abc',
            status: 'running',
            restart_count: 0,
            oom_killed: false,
            exit_code: 0,
            started_at: null,
            finished_at: null,
            image: null,
            resource_limits: {
              memory_mb: 32768,
              memory_swap_mb: null,
              nano_cpus: nanoCpus,
              cpu_shares: null,
              shm_size_mb: null,
            },
          },
        ],
      }),
    'GET /external-services/5/stats': () =>
      stats === 'fails'
        ? jsonResponse({ title: 'Docker unavailable' }, 500)
        : jsonResponse({
            service_id: 5,
            topology: 'standalone',
            members: stats === 'ok' ? [member] : [],
          }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ServiceResourcesPanel serviceId={5} serviceName="service-postgres" />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return client
}

/** Resolves once every query has its first response (data or error). */
async function queriesSettled(client: QueryClient) {
  await waitFor(() => {
    const queries = client.getQueryCache().getAll()
    expect(queries.length).toBeGreaterThan(0)
    for (const query of queries) {
      expect(query.state.status).not.toBe('pending')
    }
  })
}

test('the live CPU meter reads cores against the limit, like the Monitoring tile', async () => {
  renderPanel(6_000_000_000, 14)

  expect(await screen.findByText('0.14 / 6 cores')).toBeTruthy()
  const hint = screen.getByText('live')
  expect(hint.parentElement?.getAttribute('title')).toContain('one-second')
})

test('an uncapped container reads cores without a denominator', async () => {
  renderPanel(null, 89)

  expect(await screen.findByText('0.89 cores')).toBeTruthy()
})

test('a member missing from the stats response is not labelled live', async () => {
  const client = renderPanel(6_000_000_000, 14, 'member-missing')

  await queriesSettled(client)
  await screen.findByText('service-postgres')
  expect(screen.queryByText('live')).toBeNull()
})

test('a failed stats request is not labelled live', async () => {
  const client = renderPanel(6_000_000_000, 14, 'fails')

  await queriesSettled(client)
  await screen.findByText('service-postgres')
  expect(screen.queryByText('live')).toBeNull()
})

test('a healthy panel stays live between polls', async () => {
  renderPanel(6_000_000_000, 14)
  await screen.findByText('live')

  // Past the old 4s staleTime, before the next 5s poll: the reading is
  // still the current one and keeps its label.
  await new Promise((resolve) => setTimeout(resolve, 4_500))
  expect(screen.queryByText('live')).not.toBeNull()
}, 10_000)
