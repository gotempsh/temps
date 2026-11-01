// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { client } from '@/api/client/client.gen'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from '@/components/project-frame/project-environment-context'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import TracesList from './TracesList'

installBrowserWindow()
const originalConfig = client.getConfig()
afterEach(() => {
  cleanup()
  client.setConfig(originalConfig)
})

/** Shows the current query string, so the test can read the URL. */
function Location() {
  return <output data-testid="search">{useLocation().search}</output>
}

test('the traces list keeps the environment the bar put in the URL', async () => {
  const requests: URL[] = []
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      (input: RequestInfo | URL) => {
        requests.push(new URL(input instanceof Request ? input.url : input))
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
  const staging = {
    id: 5,
    name: 'staging',
    slug: 'staging',
  } as EnvironmentResponse
  const bar = {
    environments: [staging],
    selection: { environment: staging, all: false, source: 'url' },
    select: () => {},
  } as unknown as ProjectEnvironmentContextValue

  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/traces?env=staging&status=ERROR']}>
        <BreadcrumbProvider>
          <ProjectEnvironmentContext.Provider value={bar}>
            <TracesList
              project={{ id: 17, slug: 'synthetic' } as ProjectResponse}
            />
            <Location />
          </ProjectEnvironmentContext.Provider>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )

  // The page rewrites its own filters into the URL; the bar's `env` stays.
  const search = () => view.getByTestId('search').textContent ?? ''
  await waitFor(() => expect(search()).toContain('status=ERROR'))
  expect(new URLSearchParams(search()).get('env')).toBe('staging')
  await waitFor(() =>
    expect(
      requests.some((url) => url.searchParams.get('environment_id') === '5')
    ).toBe(true)
  )
})
