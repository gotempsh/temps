// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import {
  ServiceScopeProvider,
  type ServiceScope,
} from '@/contexts/ServiceScopeContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SavedLogSearches } from './SavedLogSearches'

installBrowserWindow()
afterEach(cleanup)

const services = [
  { id: 4, name: 'api', slug: 'api', source_type: 'git' },
  { id: 5, name: 'web', slug: 'web', source_type: 'git' },
]

function mount(
  props: {
    projectId?: number
    openedId?: number
    onPickService?: (serviceId: number) => void
  },
  wrap: (children: ReactNode) => ReactNode = (children) => children
) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        {wrap(
          <SavedLogSearches
            projectId={props.projectId}
            currentQuery={{}}
            currentRange="24h"
            onOpen={() => {}}
            onPickService={props.onPickService ?? (() => {})}
            openedId={props.openedId}
          />
        )}
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const trigger = (view: ReturnType<typeof render>) =>
  view.getByRole('button', { name: 'Saved searches' })

test('asks the server nothing until it is opened', () => {
  const api = mockApi({})
  const view = mount({ projectId: 4 })
  expect(trigger(view).getAttribute('aria-expanded')).toBe('false')
  expect(api.calls).toEqual([])
})

test('without a service it lists the services, and one click picks one', async () => {
  mockApi({
    'GET /projects': () =>
      jsonResponse({ projects: services, total: 2, page: 1, per_page: 100 }),
  })
  const picked: number[] = []
  const view = mount({ onPickService: (id) => picked.push(id) })
  fireEvent.click(trigger(view))

  const list = await view.findByRole('list', { name: 'Services' })
  expect(within(list).getAllByRole('button')).toHaveLength(2)
  expect(within(list).getByRole('button', { name: 'api' })).toBeTruthy()
  // Choosing is a click, never an instruction to type a filter.
  expect(view.baseElement.textContent).not.toContain('project:')

  fireEvent.click(within(list).getByRole('button', { name: 'web' }))
  expect(picked).toEqual([5])
})

test('says when more services exist than it lists', async () => {
  mockApi({
    'GET /projects': () =>
      jsonResponse({ projects: services, total: 140, page: 1, per_page: 100 }),
  })
  const view = mount({})
  fireEvent.click(trigger(view))
  expect(await view.findByText(/Showing the first 2/)).toBeTruthy()
})

test("inside a project it offers that project's services without asking", async () => {
  const api = mockApi({})
  const scope: ServiceScope = {
    label: 'Shop',
    href: '/p/shop',
    services: [services[0]],
    serverWide: { label: 'All logs', title: 'Every service', url: '/logs' },
  }
  const view = mount({}, (children) => (
    <ServiceScopeProvider value={scope}>{children}</ServiceScopeProvider>
  ))
  fireEvent.click(trigger(view))

  const list = await view.findByRole('list', { name: 'Services' })
  expect(within(list).getAllByRole('button')).toHaveLength(1)
  expect(api.calls.some((call) => call.path === '/projects')).toBe(false)
})

test('with a service it names the service and marks the search on screen', async () => {
  const search = (id: number, name: string) => ({
    id,
    name,
    pinned: false,
    alert_count: 0,
    project_id: 4,
    query: {},
    time_range: '24h',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  })
  mockApi({
    'GET /projects/4': () => jsonResponse(services[0]),
    'GET /projects/4/logs/saved-searches': () =>
      jsonResponse({
        data: [search(1, 'Checkout errors'), search(2, 'Slow requests')],
        page: 1,
        page_size: 100,
        total: 2,
      }),
  })
  const view = mount({ projectId: 4, openedId: 2 })
  fireEvent.click(trigger(view))

  const list = await view.findByRole('list', { name: 'Saved searches' })
  const current = list.querySelectorAll('[aria-current="true"]')
  expect(current).toHaveLength(1)
  expect(current[0].textContent).toContain('Slow requests')
  // Whose searches these are, and where "Save current search" will put one.
  expect(await view.findByText('api')).toBeTruthy()
  expect(view.getByRole('button', { name: 'Save current search' })).toBeTruthy()
})

test('failing to load the services offers a retry, not an empty list', async () => {
  mockApi({
    'GET /projects': () => jsonResponse({ title: 'Boom' }, 500),
  })
  const view = mount({})
  fireEvent.click(trigger(view))
  expect(await view.findByText('Services could not be loaded.')).toBeTruthy()
  expect(view.getByRole('button', { name: 'Retry' })).toBeTruthy()
})
