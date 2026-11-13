// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client'
import { moveWindows } from '@/lib/refresh'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { AnalyticsMetrics } from './AnalyticsMetrics'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 17, slug: 'demo', name: 'demo' } as ProjectResponse
const dayStart = new Date('2026-01-01T00:00:00Z')
const dayEnd = new Date('2026-01-02T00:00:00Z')
const moved = (date: Date) => new Date(date.getTime() + 5 * 60_000)

test("the returning share never divides the previous window's returning visitors by the moved window's visitors", async () => {
  let failedReads = 0
  mockApi({
    'GET /projects/17/unique-counts': (request) => {
      const query = new URL(request.url).searchParams
      const isMoved = query.get('end_date') !== dayEnd.toISOString()
      const metric = query.get('metric')
      if (metric === 'visitors')
        return jsonResponse({ count: isMoved ? 200 : 100 })
      if (metric === 'returning_visitors') {
        // The moved window's read fails; the previous window had 40.
        if (!isMoved) return jsonResponse({ count: 40 })
        failedReads += 1
        return jsonResponse({ detail: 'unavailable' }, 503)
      }
      return jsonResponse({ count: 1 })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = (start: Date, end: Date) => (
    <QueryClientProvider client={client}>
      <AnalyticsMetrics
        project={project}
        startDate={start}
        endDate={end}
        environment={undefined}
      />
    </QueryClientProvider>
  )
  const { rerender } = render(view(dayStart, dayEnd))
  await waitFor(() => expect(screen.getByText('40% returning')).toBeTruthy())

  moveWindows(() => rerender(view(moved(dayStart), moved(dayEnd))))

  await waitFor(() =>
    expect(screen.getAllByText('200').length).toBeGreaterThan(0)
  )
  await waitFor(() => expect(failedReads).toBe(1))
  // 40 returning of the first window over 200 visitors of the moved one.
  expect(screen.queryByText('20% returning')).toBeNull()
  expect(screen.queryByText('40% returning')).toBeNull()
})
