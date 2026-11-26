// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
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
import type { ProjectGroupResponse } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { GroupDatabasesPage } from './GroupDatabasesPage'

installBrowserWindow()
afterEach(cleanup)

const AT = '2026-01-01T00:00:00Z'
const member = (id: number, name: string, extra: object = {}) => ({
  id,
  name,
  slug: name,
  hosting: 'hosted',
  source_type: 'git',
  project_type: 'server',
  ...extra,
})
const web = member(11, 'web')
const api = member(12, 'api')
const legacy = member(13, 'legacy', {
  hosting: 'external',
  source_type: 'external',
})
const project = (...projects: object[]) =>
  ({ id: 3, name: 'Shop', slug: 'shop', projects }) as ProjectGroupResponse

const database = (id: number, name: string, service_type: string) => ({
  id,
  name,
  service_type,
  status: 'running',
  topology: 'single',
  version: service_type === 'postgres' ? '17' : '7',
  created_at: AT,
  updated_at: AT,
})
const orders = database(7, 'orders-db', 'postgres')
const cache = database(8, 'cache', 'redis')
const sessions = database(9, 'sessions', 'redis')
const link = (id: number, service: object, mode = 'project_environment') => ({
  id,
  service,
  project: { id: 0, name: '', slug: '' },
  database_provisioning_mode: mode,
})

function renderPage(group: ProjectGroupResponse, routes = {}) {
  const mock = mockApi({
    'GET /external-services/projects/11': () =>
      jsonResponse([link(1, orders), link(2, cache)]),
    'GET /external-services/projects/12': () =>
      jsonResponse([link(3, orders, 'project')]),
    'GET /external-services': () => jsonResponse([orders, cache, sessions]),
    ...routes,
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <BreadcrumbProvider>
          <GroupDatabasesPage group={group} />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return mock
}

async function openActions(name: string) {
  await act(async () => {
    fireEvent.pointerDown(
      screen.getByRole('button', { name: `Actions for ${name}` }),
      { button: 0, ctrlKey: false }
    )
  })
}

describe("a project's databases", () => {
  test('lists each database once, with every app that uses it and how', async () => {
    const mock = renderPage(project(web, api, legacy))
    const row = await screen.findByRole('button', {
      name: 'orders-db, used by web, api',
    })
    const usedBy = within(row).getByRole('list', { name: 'Used by' })
    expect(usedBy.textContent).toContain('web')
    expect(usedBy.textContent).toContain('Per environment')
    expect(usedBy.textContent).toContain('api')
    expect(usedBy.textContent).toContain('Per service')
    expect(within(row).getByText('postgres 17')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'cache, used by web' }))
    expect(screen.getByText("2 databases used by this project's services."))
    // A monitored service cannot use a database, so it is not asked.
    expect(mock.called('GET', '/external-services/projects/13')).toBe(false)
    mock.restore()
  })

  test('unlinks a shared database from one app only, after confirming', async () => {
    let unlinked = ''
    const mock = renderPage(project(web, api), {
      'DELETE /external-services/7/projects/12': () => {
        unlinked = 'api'
        return jsonResponse({})
      },
    })
    await screen.findByRole('button', { name: 'orders-db, used by web, api' })
    await openActions('orders-db')
    const unlinkGroup = screen.getByText('Unlink from').parentElement
    if (!unlinkGroup) throw new Error('no Unlink from group')
    const apiItem = within(unlinkGroup).getByRole('menuitem', { name: 'api' })
    // Each service is shown with its icon, which stays out of its name.
    expect(apiItem.querySelector('span[aria-hidden="true"]')).toBeTruthy()
    fireEvent.click(apiItem)
    expect(await screen.findByText(/api stops receiving/)).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Unlink' }))
    await waitFor(() => expect(unlinked).toBe('api'))
    expect(mock.called('DELETE', '/external-services/7/projects/11')).toBe(
      false
    )
    mock.restore()
  })

  test('links an existing database to the app chosen', async () => {
    let body: unknown
    const mock = renderPage(project(web, api), {
      'POST /external-services/9/projects': async (request: Request) => {
        body = await request.json()
        return jsonResponse(link(4, sessions))
      },
    })
    await screen.findByRole('button', { name: 'cache, used by web' })
    const apiOption = screen.getByRole('radio', { name: 'api' })
    expect(apiOption.querySelector('span[aria-hidden="true"]')).toBeTruthy()
    fireEvent.click(apiOption)
    // Listed with no click to show them. orders-db is already linked to api; cache and sessions are not.
    expect(await screen.findByText('sessions')).toBeTruthy()
    expect(
      screen
        .getAllByRole('button', { name: 'Link' })
        .map((button) => button.title)
    ).toEqual(['Link cache to api', 'Link sessions to api'])
    fireEvent.click(
      screen
        .getAllByRole('button', { name: 'Link' })
        .find((button) => button.title === 'Link sessions to api') as Element
    )
    await waitFor(() => expect(body).toEqual({ project_id: 12 }))
    mock.restore()
  })

  test('a server database asks which database to use before linking', async () => {
    const mock = renderPage(project(web, api), {
      'GET /external-services': () =>
        jsonResponse([orders, database(10, 'reports', 'postgres')]),
    })
    await screen.findByRole('button', { name: 'cache, used by web' })
    await screen.findByText('reports')
    fireEvent.click(
      screen
        .getAllByRole('button', { name: 'Link' })
        .find((button) => button.title === 'Link reports to web') as Element
    )
    expect(await screen.findByRole('dialog')).toBeTruthy()
    expect(mock.called('POST', '/external-services/10/projects')).toBe(false)
    mock.restore()
  })

  test('linking is not offered when the server has no other database', async () => {
    const mock = renderPage(project(web, api), {
      'GET /external-services': () => jsonResponse([orders]),
    })
    await screen.findByRole('button', { name: 'cache, used by web' })
    await waitFor(() =>
      expect(mock.called('GET', '/external-services')).toBe(true)
    )
    expect(screen.queryByText('Link an existing database')).toBeNull()
    mock.restore()
  })

  test('a project of monitored services only says why it has none', () => {
    const mock = renderPage(project(legacy))
    expect(
      screen.getByText("None of this project's services can use a database")
    )
    expect(mock.calls).toHaveLength(0)
    mock.restore()
  })
})

test('each service using a database links to that service', async () => {
  const mock = renderPage(project(web, api))
  const row = await screen.findByRole('button', {
    name: 'orders-db, used by web, api',
  })
  const usedBy = within(row).getByRole('list', { name: 'Used by' })
  expect(
    within(usedBy)
      .getAllByRole('link')
      .map((link) => [link.textContent, link.getAttribute('href')])
  ).toEqual([
    ['web', '/projects/web'],
    ['api', '/projects/api'],
  ])
  mock.restore()
})
