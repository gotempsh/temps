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
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { MonitorResponse, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectMonitors } from './ProjectMonitors'

installBrowserWindow()
afterEach(cleanup)

const monitor = {
  id: 5,
  name: 'Homepage check',
  monitor_type: 'web',
  monitor_url: 'https://app.example.test/',
  check_interval_seconds: 60,
  created_at: '2026-01-01T00:00:00Z',
  is_active: true,
  is_managed: false,
  environment_id: 1,
} as MonitorResponse

test('Refresh moves each monitor card to the last 24 hours ending now', async () => {
  const statusEnds: string[] = []
  mockApi({
    'GET /projects/17/monitors': () => jsonResponse([monitor]),
    'GET /projects/17/environments': () => jsonResponse([]),
    'GET /monitors/5/bucketed': (request) => {
      statusEnds.push(new URL(request.url).searchParams.get('end_time')!)
      return jsonResponse({ buckets: [] })
    },
    'GET /monitors/5/current-status': () =>
      jsonResponse({ current_status: 'operational', uptime_percentage: 100 }),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ProjectMonitors
          project={
            {
              id: 17,
              slug: 'demo',
              preset: 'docker-compose',
            } as ProjectResponse
          }
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByText('Homepage check')
  await waitFor(() => expect(statusEnds).toHaveLength(1))
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await screen.findByText('Updated just now')
  expect(statusEnds).toHaveLength(2)
  const [first, moved] = statusEnds
  expect(Date.parse(moved)).toBeGreaterThan(Date.parse(first))
  expect(screen.getByText('Homepage check')).toBeTruthy()
})
