// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { client as apiClient } from '@/api/client/client.gen'
import { getEnvironmentOptions } from '@/api/client/@tanstack/react-query.gen'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { EnvironmentDashboard } from './EnvironmentDashboard'

installBrowserWindow()
const original = apiClient.getConfig()
afterEach(() => {
  cleanup()
  apiClient.setConfig(original)
})

test('the environment name is left to the project bar and "New environment" sits in the tab row', () => {
  // Every other read of the page (containers, metrics) gets an empty list.
  apiClient.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(async () => Response.json([]), {
      preconnect: fetch.preconnect,
    }),
  })
  const environment = {
    id: 3,
    name: 'staging',
    slug: 'staging',
  } as EnvironmentResponse
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  query.setQueryData(
    getEnvironmentOptions({ path: { project_id: 17, env_id: 3 } }).queryKey,
    environment
  )
  let creates = 0
  const view = render(
    <QueryClientProvider client={query}>
      <MemoryRouter>
        <EnvironmentDashboard
          project={{ id: 17, slug: 'synthetic' } as ProjectResponse}
          environmentId={3}
          onCreateEnvironment={() => creates++}
          onDelete={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )

  // Screen readers still get the page's name; nobody sees it twice.
  const heading = view.getByRole('heading', { level: 1, name: 'staging' })
  expect(heading.className).toContain('sr-only')
  expect(
    view.queryByText('Its deployments, containers, logs and settings.')
  ).toBeNull()

  const row = view.getByRole('tablist', { name: 'staging pages' }).parentElement
    ?.parentElement
  const create = view.getByRole('button', { name: 'New environment' })
  expect(row?.contains(create)).toBe(true)
  fireEvent.click(create)
  expect(creates).toBe(1)
})
