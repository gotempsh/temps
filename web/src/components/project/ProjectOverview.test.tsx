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
