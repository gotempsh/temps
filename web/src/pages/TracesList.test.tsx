// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { client } from '@/api/client/client.gen'
import { restoreApiClient } from '@/test/api-client'
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
  restoreApiClient(originalConfig)
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

/** Renders the traces page for a service that has already received traces. */
function renderWithTraces(entry: string) {
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      (input: RequestInfo | URL) => {
        const url = new URL(input instanceof Request ? input.url : input)
        if (url.pathname.endsWith('/otel/has-traces/17'))
          return Promise.resolve(Response.json({ has_traces: true }))
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
  const bar = {
    environments: [],
    selection: { environment: null, all: true, source: 'default' },
    select: () => {},
  } as unknown as ProjectEnvironmentContextValue
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[entry]}>
        <BreadcrumbProvider>
          <ProjectEnvironmentContext.Provider value={bar}>
            <TracesList
              project={{ id: 17, slug: 'synthetic' } as ProjectResponse}
            />
          </ProjectEnvironmentContext.Provider>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a link to #traces-setup opens the setup and scrolls to it even when traces exist', async () => {
  const scrolled: string[] = []
  // Stub where the method is defined, so restoring it leaves no copy on a
  // subclass prototype that would hide other tests' stubs.
  const original = Element.prototype.scrollIntoView
  Element.prototype.scrollIntoView = function (this: Element) {
    scrolled.push(this.id)
  }
  try {
    const view = renderWithTraces('/traces#traces-setup')
    await waitFor(() =>
      expect(view.container.querySelector('#traces-setup')).not.toBeNull()
    )
    // The filter sync rewrites the URL without the hash on mount; the scroll
    // still has to happen.
    await waitFor(() => expect(scrolled).toContain('traces-setup'))
  } finally {
    Element.prototype.scrollIntoView = original
  }
})

test('without the hash a service with traces keeps the setup closed', async () => {
  const view = renderWithTraces('/traces')
  // Give the has-traces probe time to answer before asserting absence.
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(view.container.querySelector('#traces-setup')).toBeNull()
})
