// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ContainerList } from './ContainerList'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 7, slug: 'synthetic-api' } as ProjectResponse
const containers = '/projects/7/environments/3/containers'
const container = `${containers}/c-1`

/**
 * Serves one running container with live metrics. `charted` names the
 * metrics that have an hour of history (and so get a sparkline).
 */
function serveContainer(charted: string[]) {
  mockApi({
    [`GET ${containers}`]: () =>
      jsonResponse({
        containers: [
          {
            container_id: 'c-1',
            container_name: 'synthetic-api-1',
            service_name: 'synthetic-api-1',
            image_name: 'example/web:1.0',
            status: 'running',
            created_at: '2026-01-01T00:00:00Z',
          },
        ],
      }),
    [`GET ${container}/metrics`]: () =>
      jsonResponse({
        container_id: 'c-1',
        container_name: 'synthetic-api-1',
        cpu_percent: 12.5,
        cpu_limit_cores: 1,
        memory_bytes: 64 * 1024 * 1024,
        memory_limit_bytes: 512 * 1024 * 1024,
        memory_percent: 12.5,
        network_rx_bytes: 0,
        network_tx_bytes: 0,
      }),
    [`GET ${container}/metrics/history`]: (request) => {
      const metric = new URL(request.url).searchParams.get('metric') ?? ''
      return jsonResponse(
        charted.includes(metric)
          ? [{ time: '2026-01-01T00:00:00Z', value: 1 }]
          : []
      )
    },
  })
}

function renderList() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ContainerList project={project} environmentId="3" />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** The inline CPU and memory values under the container's name. */
async function inlineStats(view: ReturnType<typeof renderList>) {
  const memory = await view.findByText('64 MB')
  const cpu = view.getByText(/ cores?$/)
  return {
    cpu: cpu.closest('span.inline-flex')?.getAttribute('class') ?? '',
    memory: memory.closest('span.inline-flex')?.getAttribute('class') ?? '',
  }
}

test('a value its sparkline already charts is not repeated in the line under the name', async () => {
  serveContainer(['container.cpu_percent', 'container.memory_used_bytes'])
  const view = renderList()

  await waitFor(() =>
    expect(
      view.getAllByRole('img', { name: /usage over the last hour/ })
    ).toHaveLength(2)
  )
  await waitFor(async () => {
    const { cpu, memory } = await inlineStats(view)
    // Hidden from `lg` up, where the sparklines are drawn; shown below it.
    expect(cpu).toContain('lg:hidden')
    expect(memory).toContain('lg:hidden')
  })
})

test('a metric with no history yet keeps its value in the line', async () => {
  serveContainer(['container.cpu_percent'])
  const view = renderList()

  await waitFor(async () => {
    const { cpu, memory } = await inlineStats(view)
    expect(cpu).toContain('lg:hidden')
    // No memory sparkline: the line is the only place memory appears.
    expect(memory).not.toContain('lg:hidden')
  })
})
