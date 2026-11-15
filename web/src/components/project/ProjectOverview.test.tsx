// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { getLastDeploymentOptions } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectOverview, VisitorAnalyticsCard } from './ProjectOverview'

installBrowserWindow()
afterEach(cleanup)

function renderCard(
  overrides: Partial<Parameters<typeof VisitorAnalyticsCard>[0]>
) {
  return render(
    <MemoryRouter>
      <VisitorAnalyticsCard
        projectSlug="example"
        capabilityPending={false}
        capabilityError={false}
        enabled
        visitors={0}
        pageViews={0}
        returningVisitors={0}
        returningRate={0}
        hourlyVisitors={[]}
        loading={false}
        error={false}
        {...overrides}
      />
    </MemoryRouter>
  )
}

test('analytics that never received an event offers to set it up', () => {
  const view = renderCard({ enabled: false })

  expect(view.getByText('Add browser analytics')).toBeTruthy()
  expect(
    view.getByRole('link', { name: 'Set up analytics' }).getAttribute('href')
  ).toBe('/projects/example/analytics/setup')
  expect(view.queryByRole('link', { name: /Explore/ })).toBeNull()
})

test('a quiet day says so instead of a flat chart and four zeros', () => {
  const view = renderCard({})

  expect(view.getByText('No visitors in the last 24 hours')).toBeTruthy()
  expect(view.queryByText('Page views')).toBeNull()
  expect(view.queryByText('Returning rate')).toBeNull()
  expect(
    view.getByRole('link', { name: 'Check setup' }).getAttribute('href')
  ).toBe('/projects/example/analytics/setup')
  // Older visits are still there to explore.
  expect(view.getByRole('link', { name: /Explore/ })).toBeTruthy()
})

test('any traffic shows the numbers', () => {
  const view = renderCard({ visitors: 3, pageViews: 9 })

  expect(view.getByText('Page views')).toBeTruthy()
  expect(view.getByText('9')).toBeTruthy()
  expect(view.queryByText('No visitors in the last 24 hours')).toBeNull()
})

test('Refresh also refetches the latest deployment the project page loads', async () => {
  let lastDeploymentReads = 0
  mockApi({
    'GET /projects/7/last-deployment': () => {
      lastDeploymentReads += 1
      return jsonResponse({ deployment: null })
    },
  })
  const project = { id: 7, slug: 'example', name: 'Example' } as ProjectResponse
  // The project page owns this query and hands the overview its result.
  function ProjectPage() {
    useQuery(getLastDeploymentOptions({ path: { id: project.id } }))
    return <ProjectOverview project={project} />
  }
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ProjectPage />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await waitFor(() => expect(lastDeploymentReads).toBe(1))

  fireEvent.click(view.getByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(lastDeploymentReads).toBe(2))
})

test("the returning rate never divides the previous window's returning visitors by the moved window's visitors", async () => {
  const reads = { visitors: 0, returning_visitors: 0 }
  mockApi({
    'GET /projects/7/has-events': () => jsonResponse({ has_events: true }),
    'GET /projects/7/hourly-visits': () =>
      jsonResponse([{ date: '2026-01-01T00:00:00Z', count: 10 }]),
    'GET /projects/7/unique-counts': (request) => {
      const metric = new URL(request.url).searchParams.get('metric')
      if (metric === 'visitors')
        return jsonResponse({ count: ++reads.visitors === 1 ? 10 : 20 })
      if (metric === 'returning_visitors') {
        // The moved window's returning visitors fail; the first window had 5.
        if (++reads.returning_visitors > 1)
          return jsonResponse({ detail: 'unavailable' }, 503)
        return jsonResponse({ count: 5 })
      }
      return jsonResponse({ count: 0 })
    },
  })
  const project = { id: 7, slug: 'example', name: 'Example' } as ProjectResponse
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ProjectOverview project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await waitFor(() => expect(view.getByText('50%')).toBeTruthy())

  fireEvent.click(view.getByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(view.getByText('20')).toBeTruthy())
  await waitFor(() => expect(reads.returning_visitors).toBe(2))
  // 5 returning of the first window over 20 visitors of the moved one.
  expect(view.queryByText('25%')).toBeNull()
  expect(view.queryByText('50%')).toBeNull()
})
