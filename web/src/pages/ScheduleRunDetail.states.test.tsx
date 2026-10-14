// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, Routes, Route } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listScheduleRunJobsOptions } from '@/lib/schedule-runs'
import { ScheduleRunDetail } from './ScheduleRunDetail'

const jobsKey = listScheduleRunJobsOptions(77, 1, 200).queryKey

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/backups/schedules/5/runs/77']}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/backups/schedules/:scheduleId/runs/:runId"
              element={<ScheduleRunDetail />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an unknown run is not-found, with no cancel action and a way back', () => {
  const client = createClient()
  fail(
    client,
    jobsKey,
    Object.assign(new Error('Schedule run 77 not found'), { status: 404 })
  )
  const html = render(client)
  expect(html).toContain('Run not found')
  expect(html).toContain('href="/backups/schedules/5"')
  expect(html).toContain('href="/backups"')
  expect(html).not.toContain('Cancel run')
  client.clear()
})

test('a failed jobs read keeps the run page and explains the failure', () => {
  const client = createClient()
  fail(
    client,
    jobsKey,
    Object.assign(new Error('database unavailable'), { status: 500 })
  )
  const html = render(client)
  expect(html).not.toContain('Run not found')
  expect(html).toContain('Failed to load jobs')
  client.clear()
})
