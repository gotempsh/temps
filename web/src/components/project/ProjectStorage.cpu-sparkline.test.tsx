// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ExternalServiceInfo } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceRow } from './ProjectStorage'

installBrowserWindow()
afterEach(cleanup)

const AT = '2026-01-01T00:00:00Z'

// A cluster whose primary uses 3 cores while two replicas idle: a range query
// on `container.cpu_percent` averages the members' series to 1 core, while
// the busiest member's own series reads 3.
function renderRow(
  topology: 'standalone' | 'cluster',
  ranges: Record<string, { time: string; value: number | null }[]>,
  charted: string[] = []
) {
  mockApi({
    'GET /external-services/3/metrics': (request) => {
      const metric = new URL(request.url).searchParams.get('metric') ?? ''
      charted.push(metric)
      return jsonResponse(ranges[metric] ?? [])
    },
  })
  const service = {
    id: 3,
    name: 'db',
    service_type: 'postgres',
    status: 'running',
    topology,
    created_at: AT,
    updated_at: AT,
  } as ExternalServiceInfo
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ul>
          <ServiceRow
            service={service}
            isLinked
            isBusy={false}
            projectSlug="web"
            onToggle={async () => {}}
          />
        </ul>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('the project storage CPU sparkline', () => {
  test("a cluster reads its busiest member's cores, as its service page does", async () => {
    renderRow('cluster', {
      'container.cluster_cpu_percent': [{ time: AT, value: 300 }],
      'container.cpu_percent': [{ time: AT, value: 100 }],
    })
    expect(await screen.findByText('CPU 3.00 cores')).toBeTruthy()
    expect(screen.queryByText('CPU 1.00 cores')).toBeNull()
  })

  test("before a cluster records its busiest member, it shows the members' history", async () => {
    const charted: string[] = []
    renderRow(
      'cluster',
      {
        'container.cluster_cpu_percent': [{ time: AT, value: null }],
        'container.cpu_percent': [{ time: AT, value: 100 }],
      },
      charted
    )
    expect(await screen.findByText('CPU 1.00 cores')).toBeTruthy()
    expect(charted).toContain('container.cluster_cpu_percent')
  })

  test('a single container reads its own usage', async () => {
    const charted: string[] = []
    renderRow(
      'standalone',
      { 'container.cpu_percent': [{ time: AT, value: 100 }] },
      charted
    )
    expect(await screen.findByText('CPU 1.00 cores')).toBeTruthy()
    await waitFor(() => expect(charted).toContain('container.cpu_percent'))
    expect(charted).not.toContain('container.cluster_cpu_percent')
  })
})
