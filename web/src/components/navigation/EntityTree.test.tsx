// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { SidebarProvider } from '@/components/ui/sidebar'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EntityTree } from './EntityTree'
import { TREE_FOLD_KEY } from './use-tree-fold'

installBrowserWindow()
afterEach(() => {
  cleanup()
  window.localStorage.clear()
})

const service = (id: number, name: string) => ({
  id,
  name,
  slug: name,
  source_type: 'git',
})

/** A server with one project of two services, and one service in none. */
const projects = {
  'GET /project-groups': () =>
    jsonResponse({
      groups: [
        {
          id: 3,
          name: 'Shop',
          slug: 'shop',
          projects: [service(4, 'web'), service(5, 'api')],
        },
      ],
      total: 1,
    }),
  'GET /projects': () =>
    jsonResponse({
      projects: [service(4, 'web'), service(5, 'api'), service(9, 'blog')],
      total: 3,
    }),
  'GET /monitors-health/projects': () =>
    jsonResponse({
      projects: {
        '4': { project_id: 4, status: 'operational' },
        '5': { project_id: 5, status: 'down' },
        '9': { project_id: 9, status: 'no_monitors' },
      },
    }),
}

function renderAt(path: string, routes: Parameters<typeof mockApi>[0]) {
  const api = mockApi(routes)
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[path]}>
        <SidebarProvider>
          <EntityTree />
        </SidebarProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, view }
}

const hrefs = (root: HTMLElement) =>
  [...root.querySelectorAll('a')].map((a) => a.getAttribute('href'))

describe('the tree', () => {
  test('is what the user owns, and holds no data entry', async () => {
    const { view } = renderAt('/projects', projects)
    const tree = view.getByRole('list', { name: 'Your server' })
    await within(tree).findByRole('link', { name: /web/ })
    expect(
      [...tree.querySelectorAll(':scope > li > a')].map((a) => [
        a.textContent,
        a.getAttribute('href'),
      ])
    ).toEqual([
      ['Projects', '/projects'],
      ['Databases', '/storage'],
      ['Domains', '/domains'],
      ['Servers', '/settings/nodes'],
    ])
    for (const data of ['/logs', '/errors', '/traces', '/analytics'])
      expect(hrefs(view.container)).not.toContain(data)
    for (const parent of ['Observe', 'Resources', 'AI', 'Configure'])
      expect(view.queryByText(parent)).toBeNull()
  })

  test('Projects opens to each project and its services, then services in none', async () => {
    const { view } = renderAt('/projects', projects)
    const list = await view.findByRole('list', { name: 'Shop services' })
    expect(hrefs(list)).toEqual(['/projects/web', '/projects/api'])
    expect(
      hrefs(view.getByRole('list', { name: 'Projects' })).filter(
        (href) => !hrefs(list).includes(href)
      )
    ).toEqual(['/p/shop', '/projects/blog'])
  })

  test('a row says how its service is doing: trouble in a word, health quietly', async () => {
    const { view } = renderAt('/projects', projects)
    const api = await view.findByRole('link', { name: /api/ })
    await waitFor(() => expect(api.textContent).toContain('Down'))
    const web = view.getByRole('link', { name: /web/ })
    // Healthy is a dot, with its word for assistive technology only.
    expect(within(web).getByText('Healthy').className).toBe('sr-only')
    // A service nothing checks is not called healthy.
    const blog = view.getByRole('link', { name: /blog/ })
    expect(blog.textContent).not.toMatch(/Healthy|Down/)
  })

  test('a folded project says the worst of its services', async () => {
    window.localStorage.setItem(
      TREE_FOLD_KEY,
      JSON.stringify({ 'tree:project:3': false })
    )
    const { view } = renderAt('/projects', projects)
    const shop = await view.findByRole('link', { name: /Shop/ })
    await waitFor(() => expect(shop.textContent).toContain('Down'))
    expect(view.queryByRole('list', { name: 'Shop services' })).toBeNull()
  })

  test('asks one health answer for every service listed', async () => {
    const { api, view } = renderAt('/projects', projects)
    await view.findByRole('link', { name: /api/ })
    await waitFor(() =>
      expect(
        api.calls.filter((call) => call.path === '/monitors-health/projects')
      ).toHaveLength(1)
    )
  })
})

describe('where the user is', () => {
  test('a service is marked on any of its pages, its project opened for it', async () => {
    const { view } = renderAt('/projects/api/errors/42', projects)
    const api = await view.findByRole('link', { name: /api/ })
    expect(api.getAttribute('aria-current')).toBe('true')
    expect(
      view.getByRole('link', { name: /web/ }).getAttribute('aria-current')
    ).toBeNull()
    // Its own page is the page.
    cleanup()
    const own = renderAt('/projects/api', projects)
    expect(
      (await own.view.findByRole('link', { name: /api/ })).getAttribute(
        'aria-current'
      )
    ).toBe('page')
  })

  test('the service shown opens to its views, and the view carries the mark', async () => {
    const { view } = renderAt('/projects/api/runtime', projects)
    const views = await view.findByRole('list', { name: 'api views' })
    const titles = [...views.querySelectorAll('a')].map((a) => a.textContent)
    expect(titles.slice(0, 4)).toEqual([
      'Overview',
      'Deployments',
      'Logs',
      'Errors',
    ])
    expect(titles[titles.length - 1]).toBe('Settings')
    const logs = within(views).getByRole('link', { name: 'Logs' })
    expect(logs.getAttribute('aria-current')).toBe('page')
    expect(logs.getAttribute('data-active')).toBe('true')
    // The service reads as the one opened; the mark is on its view.
    const api = view.getByRole('link', { name: /^api/ })
    expect(api.getAttribute('aria-current')).toBe('true')
    expect(api.getAttribute('data-active')).toBe('false')
    // No other service lists its views.
    expect(view.queryByRole('list', { name: 'web views' })).toBeNull()
    expect(view.queryByRole('list', { name: 'blog views' })).toBeNull()
  })

  test('no service is opened to its views on a page that is not one of its own', async () => {
    const { view } = renderAt('/p/shop/logs', projects)
    await view.findByRole('link', { name: /api/ })
    expect(view.queryByRole('list', { name: /views$/ })).toBeNull()
  })

  test('a project is marked on its pages; folded over a service, it marks that', async () => {
    const { view } = renderAt('/p/shop/logs', projects)
    const shop = await view.findByRole('link', { name: /Shop/ })
    expect(shop.getAttribute('aria-current')).toBe('true')
    cleanup()

    window.localStorage.setItem(
      TREE_FOLD_KEY,
      JSON.stringify({ 'tree:project:3': false })
    )
    const folded = renderAt('/projects/api/logs', projects)
    expect(
      (await folded.view.findByRole('link', { name: /Shop/ })).getAttribute(
        'aria-current'
      )
    ).toBe('true')
  })

  test('data of every project is under Projects, which stays the place marked', async () => {
    window.localStorage.setItem(
      TREE_FOLD_KEY,
      JSON.stringify({ 'tree:projects': false })
    )
    const { view } = renderAt('/logs', {})
    const row = view.getByRole('link', { name: 'Projects' })
    expect(row.className).toContain('bg-sidebar-accent')
    expect(row.getAttribute('aria-current')).toBeNull()
  })
})

describe('the other branches', () => {
  const others = {
    ...projects,
    'GET /external-services': () =>
      jsonResponse([
        { id: 1, name: 'main-db', service_type: 'postgres', status: 'running' },
        { id: 2, name: 'cache', service_type: 'redis', status: 'failed' },
      ]),
    'GET /domains': () =>
      jsonResponse({
        domains: [{ id: 6, domain: 'example.test', status: 'pending_dns' }],
        total: 14,
      }),
    'GET /internal/nodes': () =>
      jsonResponse({
        nodes: [{ id: 2, name: 'worker-a', status: 'offline' }],
        total: 1,
      }),
  }

  test('stay folded, and ask nothing, until opened or shown', async () => {
    const { api, view } = renderAt('/projects', others)
    await view.findByRole('link', { name: /web/ })
    for (const path of ['/external-services', '/domains', '/internal/nodes'])
      expect(api.calls.some((call) => call.path === path)).toBe(false)

    const toggle = view.getByRole('button', { name: 'Show Databases' })
    expect(toggle.getAttribute('aria-expanded')).toBe('false')
    fireEvent.click(toggle)
    const list = await view.findByRole('list', { name: 'Databases' })
    await within(list).findByRole('link', { name: /main-db/ })
    expect(hrefs(list)).toEqual(['/storage/1', '/storage/2'])
    expect(
      within(list).getByRole('link', { name: /cache/ }).textContent
    ).toContain('Failed')
    expect(
      view
        .getByRole('button', { name: 'Hide Databases' })
        .getAttribute('aria-expanded')
    ).toBe('true')
    // The choice is kept for the next visit.
    expect(
      JSON.parse(window.localStorage.getItem(TREE_FOLD_KEY) ?? '{}')
    ).toMatchObject({ 'tree:databases': true })
  })

  test('a record opens its branch and is marked there', async () => {
    const { view } = renderAt('/storage/2/backups', others)
    const list = await view.findByRole('list', { name: 'Databases' })
    const cache = await within(list).findByRole('link', { name: /cache/ })
    expect(cache.getAttribute('aria-current')).toBe('true')
  })

  test('a long list points at its page for the rest', async () => {
    const { view } = renderAt('/domains', others)
    const list = await view.findByRole('list', { name: 'Domains' })
    const more = await within(list).findByRole('link', { name: '13 more' })
    expect(more.getAttribute('href')).toBe('/domains')
    expect(
      within(list).getByRole('link', { name: /example\.test/ }).textContent
    ).toContain('Pending')
  })

  test('a server says when it is offline', async () => {
    const { view } = renderAt('/settings/nodes', others)
    const list = await view.findByRole('list', { name: 'Servers' })
    const worker = await within(list).findByRole('link', { name: /worker-a/ })
    expect(worker.getAttribute('href')).toBe('/settings/nodes/2')
    expect(worker.textContent).toContain('Offline')
  })

  test('an empty branch offers the one thing that fills it', async () => {
    const { view } = renderAt('/storage', {
      ...projects,
      'GET /external-services': () => jsonResponse([]),
    })
    const list = await view.findByRole('list', { name: 'Databases' })
    const add = await within(list).findByRole('link', { name: 'Add database' })
    expect(add.getAttribute('href')).toBe('/storage/create')
  })

  test('a branch that could not be loaded says so and offers a retry', async () => {
    let fail = true
    const { view } = renderAt('/storage', {
      ...projects,
      'GET /external-services': () =>
        fail ? jsonResponse({ title: 'Boom' }, 500) : jsonResponse([]),
    })
    const list = await view.findByRole('list', { name: 'Databases' })
    expect(await within(list).findByText('Could not be loaded')).toBeTruthy()
    // Never an empty list standing in for an answer that did not come.
    expect(
      within(list).queryByRole('link', { name: 'Add database' })
    ).toBeNull()
    fail = false
    fireEvent.click(within(list).getByRole('button', { name: 'Retry' }))
    expect(
      await within(list).findByRole('link', { name: 'Add database' })
    ).toBeTruthy()
  })
})
