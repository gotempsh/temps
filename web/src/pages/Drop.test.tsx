// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext-components'
import { handOffDropFiles } from '@/lib/drop-handoff'
import { Drop } from './Drop'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = {
  id: 5,
  slug: 'site',
  name: 'site',
}
const GROUP = {
  id: 7,
  name: 'Shop',
  slug: 'shop',
  description: null,
  projects: [],
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
const ENVIRONMENT = {
  id: 11,
  name: 'production',
  slug: 'production',
  is_preview: false,
  main_url: 'https://site.example.test',
}

const DROP_ROUTES = {
  'GET /platform/features': () => jsonResponse({ stateless: false }),
  'POST /drop/inspect': () =>
    jsonResponse({
      suggestedName: 'site',
      candidates: [
        {
          confidence: 'high',
          directory: '.',
          isStatic: true,
          label: 'Static site',
          preset: 'static',
          reason: 'index.html found',
        },
      ],
    }),
  'POST /projects': () => jsonResponse(PROJECT),
  'DELETE /projects/5': () => new Response(null, { status: 204 }),
  'GET /projects/5/environments': () => jsonResponse([ENVIRONMENT]),
  'POST /projects/5/upload/static': () => jsonResponse({ id: 21 }),
  'POST /projects/5/environments/11/deploy/static': () => jsonResponse({}),
}

function Probe() {
  const location = useLocation()
  return <p data-testid="path">{location.pathname}</p>
}

function page(url: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[url]}>
        <BreadcrumbProvider>
          <Drop embedded />
        </BreadcrumbProvider>
        <Probe />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/**
 * Opens the page with an `index.html` handed off (the empty-state drop),
 * waits for the static preset to be detected and presses Deploy.
 */
async function dropSiteAndDeploy(url: string) {
  handOffDropFiles([
    {
      file: new File(['<h1>hi</h1>'], 'index.html', { type: 'text/html' }),
      path: 'index.html',
    },
  ])
  const view = render(page(url))
  const deploy = await waitFor(
    () => view.getByRole('button', { name: /Deploy Static site/ }),
    { timeout: 3000 }
  )
  await act(async () => {
    fireEvent.click(deploy)
  })
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 100))
  })
  return view
}

function mutations(api: ReturnType<typeof mockApi>): string[] {
  return api.calls
    .filter((call) => call.method !== 'GET')
    .map((call) => `${call.method} ${call.path}`)
}

test('joins the project before anything is uploaded or deployed', async () => {
  const api = mockApi({
    ...DROP_ROUTES,
    'GET /project-groups/7': () => jsonResponse(GROUP),
    'PUT /project-groups/7/projects/5': () => jsonResponse(GROUP),
  })
  const view = await dropSiteAndDeploy('/projects/new?group=7')

  expect(mutations(api)).toEqual([
    'POST /drop/inspect',
    'POST /projects',
    'PUT /project-groups/7/projects/5',
    'POST /projects/5/upload/static',
    'POST /projects/5/environments/11/deploy/static',
  ])
  expect(view.getByTestId('path').textContent).toBe('/p/shop')
  api.restore()
})

test('deploys nothing and removes the service when joining the project fails', async () => {
  const api = mockApi({
    ...DROP_ROUTES,
    'GET /project-groups/7': () => jsonResponse(GROUP),
    'PUT /project-groups/7/projects/5': () =>
      jsonResponse({ title: 'Boom', detail: 'database is down' }, 500),
  })
  const view = await dropSiteAndDeploy('/projects/new?group=7')

  expect(mutations(api)).toEqual([
    'POST /drop/inspect',
    'POST /projects',
    'PUT /project-groups/7/projects/5',
    'DELETE /projects/5',
  ])
  const alert = view.getByRole('alert')
  expect(alert.textContent).toContain('could not be added to Shop')
  expect(alert.textContent).toContain('nothing was deployed')
  expect(alert.textContent).toContain('database is down')
  expect(view.getByTestId('path').textContent).toBe('/projects/new')
  api.restore()
})
