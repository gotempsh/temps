// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceResourcesPanel } from './ServiceResourcesPanel'

installBrowserWindow()
afterEach(cleanup)

function renderPanel(nanoCpus: number | null, cpuPercent: number) {
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
      jsonResponse({
        service_id: 5,
        topology: 'standalone',
        members: [
          {
            role: 'primary',
            container_name: 'service-postgres',
            cpu_percent: cpuPercent,
            memory_usage_bytes: 8_870_000_000,
            memory_limit_bytes: 34_359_738_368,
            memory_percent: 25.8,
            online_cpus: 16,
          },
        ],
      }),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ServiceResourcesPanel serviceId={5} serviceName="service-postgres" />
      </MemoryRouter>
    </QueryClientProvider>
  )
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
