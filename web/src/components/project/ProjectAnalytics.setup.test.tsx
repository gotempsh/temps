// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'

// The globe pulls in three.js, which the test runner cannot load.
mock.module('@/components/analytics/EarthGlobe', () => ({
  EarthGlobe: () => null,
}))
const { AnalyticsSetup } = await import('./ProjectAnalytics')

installBrowserWindow()
afterEach(cleanup)

const HAS_EVENTS_PATH = '/projects/7/has-events'
const COUNTS_PATH = '/projects/7/unique-counts'

const project = { id: 7, slug: 'example' } as ProjectResponse

async function reachVerifyStep(hasEvents: boolean, newEvents: number) {
  const api = mockApi({
    [`GET ${HAS_EVENTS_PATH}`]: () => jsonResponse({ has_events: hasEvents }),
    [`GET ${COUNTS_PATH}`]: () => jsonResponse({ count: newEvents }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <AnalyticsSetup project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  fireEvent.click(view.getByRole('button', { name: /continue/i }))
  // Let the "has events" answer arrive before the check starts, as it does.
  await new Promise((resolve) => setTimeout(resolve, 50))
  fireEvent.click(await view.findByRole('button', { name: /start listening/ }))
  return { view, api }
}

test('checking a project that already had events does not report success by itself', async () => {
  const { view, api } = await reachVerifyStep(true, 0)

  await view.findByText('Waiting for a new event…')
  await new Promise((resolve) => setTimeout(resolve, 100))
  expect(view.queryByText(/event received/)).toBeNull()
  // What passes it (see `analyticsCheckPassed`) is what it is asking for.
  expect(api.calls.some((call) => call.path === COUNTS_PATH)).toBe(true)
})

test('a project with no events yet still waits for its first', async () => {
  const { view, api } = await reachVerifyStep(false, 0)

  await view.findByText('Waiting for your first event…')
  expect(api.calls.some((call) => call.path === COUNTS_PATH)).toBe(false)
})
