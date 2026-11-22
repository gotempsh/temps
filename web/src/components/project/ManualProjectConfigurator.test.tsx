// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, within } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { ManualProjectConfigurator } from './ManualProjectConfigurator'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 5, slug: 'web', name: 'web' }
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
        <ManualProjectConfigurator />
        <Probe />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function inputByPlaceholder(placeholder: string): HTMLInputElement {
  const input = document.body.querySelector<HTMLInputElement>(
    `input[placeholder="${placeholder}"]`
  )
  if (!input) throw new Error(`no input with placeholder ${placeholder}`)
  return input
}

async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
}

async function createServiceWithImage(view: ReturnType<typeof render>) {
  // The form refuses to submit until the platform variables have loaded.
  await settle()
  await act(async () => {
    fireEvent.change(inputByPlaceholder('my-awesome-project'), {
      target: { value: 'web' },
    })
    fireEvent.change(
      inputByPlaceholder('nginx:latest or ghcr.io/org/image:tag'),
      { target: { value: 'nginx:latest' } }
    )
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: /create service/i }))
  })
  await settle()
}

/** Routes every render of the form needs, whatever the test is about. */
const FORM_ROUTES = {
  'GET /deployments/managed-environment-variables': () => jsonResponse([]),
}

test('joins the project before the first image deployment is started', async () => {
  const api = mockApi({
    ...FORM_ROUTES,
    'GET /project-groups/7': () => jsonResponse(GROUP),
    'POST /projects': () => jsonResponse(PROJECT),
    'PUT /project-groups/7/projects/5': () => jsonResponse(GROUP),
    'GET /projects/5/environments': () => jsonResponse([ENVIRONMENT]),
    'POST /projects/5/environments/11/deploy/image': () =>
      jsonResponse({ id: 99 }),
  })
  const view = render(page('/projects/new?group=7'))

  await createServiceWithImage(view)

  const order = api.calls
    .filter((call) => call.method !== 'GET')
    .map((call) => `${call.method} ${call.path}`)
  expect(order).toEqual([
    'POST /projects',
    'PUT /project-groups/7/projects/5',
    'POST /projects/5/environments/11/deploy/image',
  ])
  expect(view.getByTestId('path').textContent).toBe('/p/shop')
  api.restore()
})

test('starts no deployment when joining the project fails', async () => {
  const api = mockApi({
    ...FORM_ROUTES,
    'GET /project-groups/7': () => jsonResponse(GROUP),
    'POST /projects': () => jsonResponse(PROJECT),
    'PUT /project-groups/7/projects/5': () =>
      jsonResponse({ title: 'Boom', detail: 'database is down' }, 500),
    'GET /projects/5/environments': () => jsonResponse([ENVIRONMENT]),
    'POST /projects/5/environments/11/deploy/image': () =>
      jsonResponse({ id: 99 }),
  })
  const view = render(page('/projects/new?group=7'))

  await createServiceWithImage(view)

  expect(api.called('PUT', '/project-groups/7/projects/5')).toBe(true)
  expect(api.called('POST', '/projects/5/environments/11/deploy/image')).toBe(
    false
  )
  expect(api.called('GET', '/projects/5/environments')).toBe(false)
  // The service exists and the failure is on screen, not swallowed.
  expect(
    within(document.body).getByText(/web was created, but not added to Shop/)
  ).toBeTruthy()
  expect(view.getByTestId('path').textContent).toBe('/projects/new')
  api.restore()
})

test('deploys the image directly when the service has no target project', async () => {
  const api = mockApi({
    ...FORM_ROUTES,
    'POST /projects': () => jsonResponse(PROJECT),
    'GET /projects/5/environments': () => jsonResponse([ENVIRONMENT]),
    'POST /projects/5/environments/11/deploy/image': () =>
      jsonResponse({ id: 99 }),
  })
  const view = render(page('/projects/new'))

  await createServiceWithImage(view)

  expect(api.calls.some((call) => call.method === 'PUT')).toBe(false)
  expect(api.called('POST', '/projects/5/environments/11/deploy/image')).toBe(
    true
  )
  expect(view.getByTestId('path').textContent).toBe('/projects/web/deployments')
  api.restore()
})
