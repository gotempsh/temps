// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { act, cleanup, render, waitFor } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client'
import {
  getLastDeploymentQueryKey,
  getProjectBySlugQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { SampleDeployTracker } from './SampleDeployTracker'

installBrowserWindow()
afterEach(cleanup)

const project = {
  id: 42,
  name: 'hello-temps',
  slug: 'hello-temps',
  source_type: 'manual',
} as unknown as ProjectResponse

/**
 * A project with no deployment answers `null` to every poll. Polls 2..5 change
 * none of the fields the component used to read (`data`, `isPending`,
 * `errorUpdateCount`), so React Query did not re-render it and it stayed on the
 * skeleton after the bounded polling had ended.
 */
test('leaves the skeleton once repeated empty answers have used up the polls', async () => {
  const api = mockApi({
    'GET /projects/42/last-deployment': () => jsonResponse(null),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getProjectBySlugQueryKey({ path: { slug: project.slug } }),
    project
  )

  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SampleDeployTracker projectSlug={project.slug} />
      </MemoryRouter>
    </QueryClientProvider>
  )

  const lastDeploymentKey = getLastDeploymentQueryKey({
    path: { id: project.id },
  })
  await waitFor(() =>
    expect(client.getQueryState(lastDeploymentKey)?.dataUpdateCount).toBe(1)
  )
  // Still inside the bounded waiting window: no verdict yet.
  expect(view.queryByText('No deployment has started yet')).toBeNull()

  for (let poll = 2; poll <= 5; poll++) {
    await act(async () => {
      await client.refetchQueries({ queryKey: lastDeploymentKey })
    })
  }

  await waitFor(() =>
    expect(view.getByText('No deployment has started yet')).toBeTruthy()
  )
  api.restore()
})
