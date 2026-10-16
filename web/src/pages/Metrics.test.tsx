// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import type { OtelDashboardResponse, ProjectResponse } from '@/api/client'
import { listDashboardsOptions } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import Metrics from './Metrics'

function Where() {
  const { pathname, search, hash } = useLocation()
  return <output data-testid="where">{`${pathname}${search}${hash}`}</output>
}

installBrowserWindow()
afterEach(cleanup)

const project = { id: 3, slug: 'web' } as ProjectResponse

function renderAt(entry: string, client: QueryClient) {
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          {/* Static routes outrank the splat, so the redirect's landing
              spot is observable without rendering the real pages. */}
          <Route path="/projects/web/metrics/explore" element={<Where />} />
          <Route
            path="/projects/web/metrics/dashboards/:id"
            element={<Where />}
          />
          <Route
            path="/projects/web/metrics/*"
            element={<Metrics project={project} />}
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an Explore deep link keeps its query string and hash', () => {
  const view = renderAt(
    '/projects/web/metrics?metric=cpu&a=b#c',
    new QueryClient()
  )
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/metrics/explore?metric=cpu&a=b#c'
  )
})

test('the dashboard-first redirect keeps the query string and hash', () => {
  const client = new QueryClient()
  client.setQueryData(
    listDashboardsOptions({ query: { project_id: project.id } }).queryKey,
    { data: [{ id: 9 } as OtelDashboardResponse], total: 1 }
  )
  const view = renderAt('/projects/web/metrics?a=b#c', client)
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/metrics/dashboards/9?a=b#c'
  )
})

test('the explore fallback keeps the query string and hash', () => {
  const client = new QueryClient()
  client.setQueryData(
    listDashboardsOptions({ query: { project_id: project.id } }).queryKey,
    { data: [], total: 0 }
  )
  const view = renderAt('/projects/web/metrics?a=b#c', client)
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/metrics/explore?a=b#c'
  )
})
