// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectStorage } from './ProjectStorage'

installBrowserWindow()
afterEach(cleanup)

const AT = '2026-01-01T00:00:00Z'
const linked = {
  id: 100,
  service: {
    id: 7,
    name: 'orders-db',
    service_type: 'postgres',
    status: 'running',
    created_at: AT,
    updated_at: AT,
  },
}

test("a failed server-wide picker keeps the service's linked databases", async () => {
  let serverCalls = 0
  mockApi({
    'GET /external-services/projects/11': () => jsonResponse([linked]),
    'GET /external-services': () => {
      serverCalls += 1
      return jsonResponse({ title: 'Unavailable' }, 500)
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <BreadcrumbProvider>
          <ProjectStorage
            project={{ id: 11, slug: 'web', name: 'web' } as ProjectResponse}
          />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(await screen.findByText('orders-db')).toBeTruthy()
  // Asked for with the page, and shown even though it failed, so its retry
  // is reachable.
  expect(
    await screen.findByText('The databases on this server could not be loaded')
  ).toBeTruthy()
  expect(serverCalls).toBe(1)
  // The working collection stays, with no page-level failure.
  expect(screen.getByText('orders-db')).toBeTruthy()
  expect(screen.queryByText('Databases could not be loaded')).toBeNull()

  const pickerAlert = screen
    .getByText('The databases on this server could not be loaded')
    .closest('[role="alert"]') as HTMLElement
  fireEvent.click(within(pickerAlert).getByRole('button', { name: 'Retry' }))
  await waitFor(() => expect(serverCalls).toBe(2))
})

function renderStorage(serverDatabases: unknown[]) {
  const api = mockApi({
    'GET /external-services/projects/11': () => jsonResponse([linked]),
    'GET /external-services': () => jsonResponse(serverDatabases),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <BreadcrumbProvider>
          <ProjectStorage
            project={{ id: 11, slug: 'web', name: 'web' } as ProjectResponse}
          />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return api
}

test('linking is not offered when every database on the server is already linked', async () => {
  const api = renderStorage([linked.service])
  expect(await screen.findByText('orders-db')).toBeTruthy()
  await waitFor(() =>
    expect(api.called('GET', '/external-services')).toBe(true)
  )
  expect(screen.queryByText('Link an existing database')).toBeNull()
})

/** A database of a type the service does not use yet. */
const redis = (id: number, name: string) => ({
  ...linked.service,
  id,
  name,
  service_type: 'redis',
})

test('the databases that can be linked are listed, with no click to show them', async () => {
  renderStorage([linked.service, redis(8, 'cache-db')])
  expect(await screen.findByText('cache-db')).toBeTruthy()
  // A few need no search.
  expect(
    screen.queryByRole('textbox', { name: 'Search existing databases' })
  ).toBeNull()
})

test('a database of a type the service already uses is not offered', async () => {
  // The server refuses a second Postgres for one service.
  const api = renderStorage([
    linked.service,
    { ...linked.service, id: 8, name: 'reports-db' },
  ])
  expect(await screen.findByText('orders-db')).toBeTruthy()
  await waitFor(() =>
    expect(api.called('GET', '/external-services')).toBe(true)
  )
  expect(screen.queryByText('reports-db')).toBeNull()
  expect(screen.queryByText('Link an existing database')).toBeNull()
})

test('past six, the rest are found by search', async () => {
  renderStorage([
    linked.service,
    ...Array.from({ length: 8 }, (_, index) =>
      redis(20 + index, `db-${index}`)
    ),
  ])
  expect(await screen.findByText('db-0')).toBeTruthy()
  expect(screen.queryByText('db-6')).toBeNull()
  expect(screen.getByText('2 more. Search to find them.')).toBeTruthy()
  fireEvent.change(
    screen.getByRole('textbox', { name: 'Search existing databases' }),
    { target: { value: 'db-7' } }
  )
  expect(await screen.findByText('db-7')).toBeTruthy()
  expect(screen.queryByText('db-0')).toBeNull()
})

test('a search typed stays editable when the list no longer needs one', async () => {
  const databases = [
    linked.service,
    ...Array.from({ length: 7 }, (_, index) =>
      redis(20 + index, `db-${index}`)
    ),
  ]
  const api = mockApi({
    'GET /external-services/projects/11': () => jsonResponse([linked]),
    'GET /external-services': () => jsonResponse(databases),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <BreadcrumbProvider>
          <ProjectStorage
            project={{ id: 11, slug: 'web', name: 'web' } as ProjectResponse}
          />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const search = await screen.findByRole('textbox', {
    name: 'Search existing databases',
  })
  fireEvent.change(search, { target: { value: 'nothing-like-it' } })
  expect(await screen.findByText('No databases match your search.'))
  // One linked elsewhere leaves six: the filter still applies, so the box
  // that clears it stays.
  databases.pop()
  await act(async () => {
    await client.refetchQueries()
    // React Query tells the page on its next tick.
    await new Promise((resolve) => setTimeout(resolve, 20))
  })
  expect(
    screen.getByRole('textbox', { name: 'Search existing databases' })
  ).toBeTruthy()
  expect(screen.getByText('No databases match your search.')).toBeTruthy()
  api.restore()
})

test('a database past the first page of the server list is offered', async () => {
  // The list pages; a full first page means there is another.
  const first = [
    linked.service,
    ...Array.from({ length: 99 }, (_, index) =>
      redis(200 + index, `cache-${index}`)
    ),
  ]
  const api = mockApi({
    'GET /external-services/projects/11': () => jsonResponse([linked]),
    'GET /external-services': (request: Request) =>
      jsonResponse(
        new URL(request.url).searchParams.get('page') === '2'
          ? [{ ...redis(400, 'late-db'), service_type: 'mongodb' }]
          : first
      ),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <BreadcrumbProvider>
          <ProjectStorage
            project={{ id: 11, slug: 'web', name: 'web' } as ProjectResponse}
          />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const search = await screen.findByRole('textbox', {
    name: 'Search existing databases',
  })
  fireEvent.change(search, { target: { value: 'late' } })
  expect(await screen.findByText('late-db')).toBeTruthy()
  api.restore()
})

// happy-dom exposes its controls on the window; the DOM lib does not type them.
const happyDOM = () =>
  (
    window as unknown as {
      happyDOM?: { setViewport(viewport: { width: number }): void }
    }
  ).happyDOM

test("a narrow window asks for no linked database's metrics; a wide one does", async () => {
  const width = window.innerWidth
  try {
    for (const [innerWidth, asked] of [
      [390, false],
      [1440, true],
    ] as const) {
      happyDOM()?.setViewport({ width: innerWidth })
      expect(window.innerWidth).toBe(innerWidth)
      const api = renderStorage([linked.service])
      expect(await screen.findByText('orders-db')).toBeTruthy()
      await waitFor(() =>
        expect(api.called('GET', '/external-services')).toBe(true)
      )
      // The sparkline has no room in a narrow row, so nothing polls for it.
      if (asked)
        await waitFor(() =>
          expect(api.called('GET', '/external-services/7/metrics')).toBe(true)
        )
      else expect(api.called('GET', '/external-services/7/metrics')).toBe(false)
      api.restore()
      cleanup()
    }
  } finally {
    happyDOM()?.setViewport({ width })
  }
})
