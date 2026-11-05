// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CreateStatusPageForm } from './CreateStatusPageForm'
import { StatusPagesList } from './ProjectStatusPages'

installBrowserWindow()
afterEach(cleanup)

const created = { id: 5, title: 'Acme status', is_published: false }

function api(routes: Parameters<typeof mockApi>[0] = {}) {
  return mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [{ id: 1, name: 'Billing', slug: 'billing' }],
        total: 1,
      }),
    'GET /projects/1/monitors': () =>
      jsonResponse([
        {
          id: 11,
          name: 'billing-production',
          environment_id: 21,
          is_managed: true,
          is_active: true,
        },
      ]),
    'GET /projects/1/environments': () =>
      jsonResponse([{ id: 21, name: 'production', is_preview: false }]),
    ...routes,
  })
}

function setup() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/status-pages/new']}>
        <Routes>
          <Route
            path="/status-pages/new"
            element={<CreateStatusPageForm initialProjectId={1} />}
          />
          <Route path="/status-pages/:id" element={<p>Page detail</p>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const typeTitle = () =>
  fireEvent.change(screen.getByLabelText('Title'), {
    target: { value: 'Acme status' },
  })

/** Opens the picker and chooses a service. */
async function pickService(name: string) {
  fireEvent.click(await screen.findByRole('combobox', { name: /^Add a/ }))
  fireEvent.click(await screen.findByRole('option', { name }))
}

async function addComponent() {
  await pickService('Billing')
  await screen.findByRole('button', { name: 'Remove Billing' })
}

test('creates and publishes, then opens the new page', async () => {
  const bodies: unknown[] = []
  const mock = api({
    'POST /status-pages': async (request) => {
      bodies.push(await request.json())
      return jsonResponse(created, 201)
    },
    'POST /status-pages/5/publish': () =>
      jsonResponse({ ...created, is_published: true }),
  })
  setup()
  typeTitle()
  await addComponent()
  fireEvent.click(screen.getByRole('button', { name: 'Create and publish' }))
  await screen.findByText('Page detail')
  expect(bodies).toEqual([
    {
      title: 'Acme status',
      monitors: [
        expect.objectContaining({ monitor_id: 11, display_name: 'Billing' }),
      ],
    },
  ])
  expect(mock.calls.map((call) => `${call.method} ${call.path}`)).toContain(
    'POST /status-pages/5/publish'
  )
  mock.restore()
})

test('a draft is created without publishing', async () => {
  const mock = api({
    'POST /status-pages': () => jsonResponse(created, 201),
  })
  setup()
  typeTitle()
  await addComponent()
  fireEvent.click(screen.getByRole('radio', { name: 'Save as draft' }))
  expect(
    await screen.findByText('The page stays private until you publish it.')
  ).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: 'Create draft' }))
  await screen.findByText('Page detail')
  expect(
    mock.calls.some((call) => call.path === '/status-pages/5/publish')
  ).toBe(false)
  mock.restore()
})

test('a page needs a component, and nothing is sent without one', async () => {
  const mock = api()
  setup()
  typeTitle()
  await waitFor(() =>
    expect(
      (
        screen.getByRole('button', {
          name: 'Create and publish',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(false)
  )
  fireEvent.click(screen.getByRole('button', { name: 'Create and publish' }))
  await screen.findByText('Add at least one component')
  expect(mock.calls.some((call) => call.method === 'POST')).toBe(false)
  mock.restore()
})

test('nothing can be created before anything is entered', () => {
  const mock = api()
  setup()
  const create = screen.getByRole('button', {
    name: 'Create and publish',
  }) as HTMLButtonElement
  expect(create.disabled).toBe(true)
  mock.restore()
})

test('a refused create stays on the form with the edits intact', async () => {
  const mock = api({
    'POST /status-pages': () => jsonResponse({ title: 'Refused' }, 409),
  })
  setup()
  typeTitle()
  await addComponent()
  fireEvent.click(screen.getByRole('button', { name: 'Create and publish' }))
  await waitFor(() =>
    expect(mock.calls.some((call) => call.method === 'POST')).toBe(true)
  )
  await waitFor(() =>
    expect((screen.getByLabelText('Title') as HTMLInputElement).value).toBe(
      'Acme status'
    )
  )
  expect(screen.queryByText('Page detail')).toBeNull()
  expect(screen.getByRole('button', { name: 'Remove Billing' })).toBeTruthy()
  mock.restore()
})

test('a publish failure still opens the draft that was created', async () => {
  const mock = api({
    'POST /status-pages': () => jsonResponse(created, 201),
    'POST /status-pages/5/publish': () =>
      jsonResponse({ title: 'Refused' }, 409),
  })
  setup()
  typeTitle()
  await addComponent()
  fireEvent.click(screen.getByRole('button', { name: 'Create and publish' }))
  await screen.findByText('Page detail')
  mock.restore()
})

const STAGING_MONITOR = {
  id: 12,
  name: 'billing-staging',
  environment_id: 22,
  is_managed: true,
  is_active: true,
}

function withStaging() {
  return api({
    'GET /projects/1/monitors': () =>
      jsonResponse([
        {
          id: 11,
          name: 'billing-production',
          environment_id: 21,
          is_managed: true,
          is_active: true,
        },
        STAGING_MONITOR,
      ]),
    'GET /projects/1/environments': () =>
      jsonResponse([
        { id: 22, name: 'staging', is_preview: true },
        { id: 21, name: 'production', is_preview: false },
      ]),
  })
}

test('choosing a service adds it, with no environment to choose when it has one', async () => {
  const mock = api()
  setup()
  await addComponent()
  expect(screen.getByText(/· production/)).toBeTruthy()
  expect((screen.getByLabelText('Public name') as HTMLInputElement).value).toBe(
    'Billing'
  )
  expect(screen.queryByText('Environment')).toBeNull()
  expect(screen.queryByRole('button', { name: 'Add' })).toBeNull()
  mock.restore()
})

test('a service with several environments starts on production and offers the rest', async () => {
  const mock = withStaging()
  setup()
  await addComponent()
  expect(screen.getByText(/· production/)).toBeTruthy()
  expect(
    await screen.findByRole('combobox', { name: /^Environment of Billing/ })
  ).toBeTruthy()
  mock.restore()
})

test('choosing the same service again adds its next environment', async () => {
  const mock = withStaging()
  setup()
  await addComponent()
  await pickService('Billing')
  await screen.findByText(/· staging/)
  expect(screen.getAllByLabelText('Public name').length).toBe(2)
  mock.restore()
})

test('a service with every environment on the page says so and adds nothing', async () => {
  const mock = api()
  setup()
  await addComponent()
  await pickService('Billing')
  await screen.findByText(/Every environment of Billing is already/)
  expect(screen.getAllByLabelText('Public name').length).toBe(1)
  mock.restore()
})

test('a service with no monitors explains why nothing was added', async () => {
  const mock = api({ 'GET /projects/1/monitors': () => jsonResponse([]) })
  setup()
  await pickService('Billing')
  await screen.findByText(/has no monitors yet/)
  expect(screen.queryByLabelText('Public name')).toBeNull()
  mock.restore()
})

test('a failed lookup can be retried', async () => {
  let attempts = 0
  const mock = api({
    'GET /projects/1/monitors': () => {
      attempts += 1
      return attempts === 1
        ? jsonResponse({ title: 'Unavailable' }, 503)
        : jsonResponse([
            {
              id: 11,
              name: 'billing-production',
              environment_id: 21,
              is_managed: true,
              is_active: true,
            },
          ])
    },
  })
  setup()
  await pickService('Billing')
  await screen.findByText('Failed to add Billing')
  fireEvent.click(screen.getByRole('button', { name: /retry|try again/i }))
  await screen.findByRole('button', { name: 'Remove Billing' })
  mock.restore()
})

test('the service the user came from is listed first', async () => {
  const mock = api({
    'GET /projects': () =>
      jsonResponse({
        projects: [
          { id: 2, name: 'Alpha', slug: 'alpha' },
          { id: 1, name: 'Billing', slug: 'billing' },
        ],
        total: 2,
      }),
  })
  setup()
  fireEvent.click(await screen.findByRole('combobox', { name: /^Add a/ }))
  const options = await screen.findAllByRole('option')
  expect(options[0].textContent).toContain('Billing')
  expect(options[1].textContent).toContain('Alpha')
  mock.restore()
})

test('the list sends you to the create page, from its empty state too', async () => {
  const mock = mockApi({
    'GET /status-pages': () => jsonResponse({ items: [], total: 0 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <StatusPagesList />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const link = await screen.findByRole('link', { name: /Create status page/ })
  expect(link.getAttribute('href')).toBe('/status-pages/new')
  mock.restore()
})

/** A response held until the test releases it. */
function gate() {
  let release: () => void = () => {}
  const opened = new Promise<void>((resolve) => (release = resolve))
  return { opened, release }
}

test('nothing can be created while a chosen service is still being added', async () => {
  const held = gate()
  const mock = api({
    'GET /projects/1/monitors': async () => {
      await held.opened
      return jsonResponse([
        {
          id: 11,
          name: 'billing-production',
          environment_id: 21,
          is_managed: true,
          is_active: true,
        },
      ])
    },
  })
  setup()
  typeTitle()
  await pickService('Billing')
  await screen.findByText('Wait for the service you chose to be added.')
  expect(
    (
      screen.getByRole('button', {
        name: 'Create and publish',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
  held.release()
  await screen.findByRole('button', { name: 'Remove Billing' })
  await waitFor(() =>
    expect(
      (
        screen.getByRole('button', {
          name: 'Create and publish',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(false)
  )
  mock.restore()
})

test('an addition that lands after a row changed environment adds what is free now', async () => {
  const lookup: { held?: ReturnType<typeof gate> } = {}
  const monitors = [
    {
      id: 11,
      name: 'billing-production',
      environment_id: 21,
      is_managed: true,
      is_active: true,
    },
    STAGING_MONITOR,
  ]
  const mock = api({
    'GET /projects/1/monitors': async () => {
      if (lookup.held) await lookup.held.opened
      return jsonResponse(monitors)
    },
    'GET /projects/1/environments': () =>
      jsonResponse([
        { id: 22, name: 'staging', is_preview: true },
        { id: 21, name: 'production', is_preview: false },
      ]),
  })
  setup()
  await addComponent()
  lookup.held = gate()
  await pickService('Billing')
  // While that lookup runs, the first row moves to staging, the environment
  // the addition would have picked.
  fireEvent.click(
    await screen.findByRole('combobox', { name: /^Environment of Billing/ })
  )
  fireEvent.click(await screen.findByRole('option', { name: /staging/ }))
  await screen.findByText(/· staging/)
  lookup.held.release()
  await screen.findByText(/· production/)
  expect(screen.getAllByLabelText('Public name').length).toBe(2)
  expect(screen.getAllByText(/· staging/).length).toBe(1)
  mock.restore()
})
