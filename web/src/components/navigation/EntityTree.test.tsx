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
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { SidebarProvider } from '@/components/ui/sidebar'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EntityTree } from './EntityTree'
import { RECENT_PROJECTS_KEY } from './use-recent-projects'
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

const mainDb = {
  id: 1,
  name: 'main-db',
  service_type: 'postgres',
  status: 'running',
}
const cache = { id: 2, name: 'cache', service_type: 'redis', status: 'failed' }
const link = (projectId: number, database: typeof mainDb) => ({
  id: projectId * 10 + database.id,
  database_provisioning_mode: 'shared',
  project: { id: projectId },
  service: database,
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
  // Both services of Shop use its database; only the API uses the cache.
  'GET /external-services/projects/4': () => jsonResponse([link(4, mainDb)]),
  'GET /external-services/projects/5': () =>
    jsonResponse([link(5, mainDb), link(5, cache)]),
}

function renderAt(path: string, routes: Parameters<typeof mockApi>[0]) {
  const api = mockApi(routes)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <SidebarProvider>
          <EntityTree />
        </SidebarProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, view, client }
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
      // What a user comes to work on, then one row for the server.
      ['Projects', '/projects'],
      ['Server', '/settings/nodes'],
    ])
    for (const data of ['/logs', '/errors', '/traces', '/analytics'])
      expect(hrefs(view.container)).not.toContain(data)
    for (const parent of ['Observe', 'Resources', 'AI', 'Configure'])
      expect(view.queryByText(parent)).toBeNull()
  })

  test('Projects opens to each project and what it is made of, then services in none', async () => {
    const { view } = renderAt('/projects', projects)
    const list = await view.findByRole('list', { name: 'Shop contents' })
    // Its services, then the databases they use, each once.
    await within(list).findByRole('link', { name: /cache/ })
    expect(hrefs(list)).toEqual([
      '/projects/web',
      '/projects/api',
      '/storage/2',
      '/storage/1',
    ])
    expect(
      within(list).getByRole('link', { name: /cache/ }).textContent
    ).toContain('Failed')
    expect(
      hrefs(view.getByRole('list', { name: 'Projects' })).filter(
        (href) => !hrefs(list).includes(href)
      )
    ).toEqual(['/p/shop', '/projects/blog'])
  })

  test('an open project offers what it can hold: a service or a database', async () => {
    const { view } = renderAt('/projects', projects)
    const add = await view.findByRole('button', { name: 'Add to Shop' })
    expect(add.textContent).toBe('Add')
    fireEvent.pointerDown(add, { button: 0, ctrlKey: false })
    const menu = await view.findByRole('menu')
    expect(
      [...menu.querySelectorAll('a')].map((a) => [
        a.textContent,
        a.getAttribute('href'),
      ])
    ).toEqual([
      ['Service', '/projects/new?group=3'],
      ['Database', '/p/shop/databases'],
    ])
  })

  test('a folded project asks nothing about its databases', async () => {
    window.localStorage.setItem(
      TREE_FOLD_KEY,
      JSON.stringify({ 'tree:project:3': false })
    )
    const { api, view } = renderAt('/projects', projects)
    await view.findByRole('link', { name: /Shop/ })
    await view.findByRole('link', { name: /blog/ })
    expect(
      api.calls.some((call) =>
        call.path.startsWith('/external-services/projects')
      )
    ).toBe(false)
  })

  test('a project whose databases could not be loaded says so, and its services stay', async () => {
    let fail = true
    const { view } = renderAt('/projects', {
      ...projects,
      'GET /external-services/projects/5': () =>
        fail
          ? jsonResponse({ title: 'Boom' }, 500)
          : jsonResponse([link(5, cache)]),
    })
    const list = await view.findByRole('list', { name: 'Shop contents' })
    expect(
      await within(list).findByText('Databases could not be loaded')
    ).toBeTruthy()
    expect(within(list).getByRole('link', { name: /api/ })).toBeTruthy()
    fail = false
    fireEvent.click(within(list).getByRole('button', { name: 'Retry' }))
    expect(
      await within(list).findByRole('link', { name: /cache/ })
    ).toBeTruthy()
    await waitFor(() =>
      expect(
        within(list).queryByText('Databases could not be loaded')
      ).toBeNull()
    )
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
    expect(view.queryByRole('list', { name: 'Shop contents' })).toBeNull()
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

describe('the Server row', () => {
  const server = {
    ...projects,
    'GET /external-services': () => jsonResponse([mainDb, cache]),
    'GET /internal/nodes': () =>
      jsonResponse({
        nodes: [
          { id: 1, name: 'control', status: 'active' },
          { id: 2, name: 'worker-a', status: 'offline' },
        ],
        total: 2,
      }),
  }
  const serverRow = (view: ReturnType<typeof renderAt>['view']) =>
    view.getByRole('link', { name: /^Server/ })

  test('says the worst of the machines and databases, counted', async () => {
    const { view } = renderAt('/projects', server)
    await waitFor(() =>
      expect(serverRow(view).textContent).toBe('Server1 offline')
    )
    cleanup()
    const healthy = renderAt('/projects', {
      ...projects,
      'GET /external-services': () => jsonResponse([mainDb]),
      'GET /internal/nodes': () =>
        jsonResponse({ nodes: [{ id: 1, name: 'a', status: 'active' }] }),
    })
    // Healthy is a dot, with its word for assistive technology only.
    await waitFor(() =>
      expect(
        within(serverRow(healthy.view)).getByText('Healthy').className
      ).toBe('sr-only')
    )
  })

  test('counts every database, not the first page of them', async () => {
    // A full first page of running databases, and a failed one after it.
    const running = Array.from({ length: 100 }, (_, index) => ({
      ...mainDb,
      id: 100 + index,
      name: `db-${index}`,
    }))
    const { view } = renderAt('/projects', {
      ...projects,
      'GET /external-services': (request: Request) =>
        jsonResponse(
          new URL(request.url).searchParams.get('page') === '2'
            ? [{ ...cache, id: 500 }]
            : running
        ),
      'GET /internal/nodes': () =>
        jsonResponse({ nodes: [{ id: 1, name: 'a', status: 'active' }] }),
    })
    await waitFor(() =>
      expect(serverRow(view).textContent).toBe('Server1 failed')
    )
  })

  test('says nothing when it cannot be asked', async () => {
    const { view } = renderAt('/projects', projects)
    await view.findByRole('link', { name: /web/ })
    expect(serverRow(view).textContent).toBe('Server')
  })

  test("is marked on any of the server's pages", async () => {
    for (const [path, current] of [
      ['/settings/nodes', 'page'],
      ['/monitoring/server', 'true'],
      ['/storage', 'true'],
      ['/backups', 'true'],
      ['/domains/4', 'true'],
      ['/certificates', 'true'],
    ]) {
      const { view } = renderAt(path, server)
      expect(serverRow(view).getAttribute('aria-current')).toBe(current)
      expect(serverRow(view).getAttribute('data-active')).toBe('true')
      cleanup()
    }
  })

  test('a database a project lists is marked there, not on the server', async () => {
    const { view } = renderAt('/storage/2/backups', server)
    const list = await view.findByRole('list', { name: 'Shop contents' })
    const row = await within(list).findByRole('link', { name: /cache/ })
    expect(row.getAttribute('aria-current')).toBe('true')
    expect(serverRow(view).getAttribute('aria-current')).toBeNull()
    cleanup()
    // One no project uses is the server's.
    const unused = renderAt('/storage/7', server)
    await unused.view.findByRole('link', { name: /cache/ })
    expect(serverRow(unused.view).getAttribute('aria-current')).toBe('true')
  })
})

describe('Projects', () => {
  /** A server of twelve projects of one service each. */
  const large = (statuses: Record<number, string> = {}) => {
    const groups = Array.from({ length: 12 }, (_, index) => ({
      id: index + 1,
      name: `Project ${index + 1}`,
      slug: `project-${index + 1}`,
      projects: [service(100 + index, `svc-${index + 1}`)],
    }))
    return {
      'GET /project-groups': () => jsonResponse({ groups, total: 12 }),
      'GET /projects': () =>
        jsonResponse({
          projects: groups.flatMap((group) => group.projects),
          total: 12,
        }),
      'GET /monitors-health/projects': () =>
        jsonResponse({
          projects: Object.fromEntries(
            Object.entries(statuses).map(([id, status]) => [
              id,
              { project_id: Number(id), status },
            ])
          ),
        }),
    }
  }
  const listed = (view: ReturnType<typeof renderAt>['view']) =>
    [
      ...view
        .getByRole('list', { name: 'Projects' })
        .querySelectorAll(':scope > li[data-project]'),
    ].map((row) => row.getAttribute('data-project'))

  test('a large server lists eight: trouble first, then those opened recently', async () => {
    window.localStorage.setItem(
      RECENT_PROJECTS_KEY,
      JSON.stringify(['tree-key-of-nothing', 'project:12'])
    )
    const { view } = renderAt('/projects', large({ 110: 'down' }))
    await view.findByRole('link', { name: /^Project 1\b/ })
    await waitFor(() => expect(listed(view)[0]).toBe('project-11'))
    expect(listed(view)).toEqual([
      'project-11',
      'project-1',
      'project-2',
      'project-3',
      'project-4',
      'project-5',
      'project-6',
      'project-12',
    ])
    const all = view.getByRole('link', { name: 'All projects (12)' })
    expect(all.getAttribute('href')).toBe('/projects')
  })

  test('the project shown is always listed, and remembered as recent', async () => {
    const { view } = renderAt('/p/project-10', large())
    await waitFor(() => expect(listed(view)).toContain('project-10'))
    expect(listed(view)).toHaveLength(8)
    expect(
      JSON.parse(window.localStorage.getItem(RECENT_PROJECTS_KEY) ?? '[]')
    ).toEqual(['project:10'])
  })

  test('a server with few projects lists them all, with no link to the rest', async () => {
    const { view } = renderAt('/projects', projects)
    await view.findByRole('link', { name: /blog/ })
    expect(view.queryByRole('link', { name: /^All projects/ })).toBeNull()
  })

  test('an empty server offers the one thing that fills it', async () => {
    const { view } = renderAt('/projects', {
      'GET /project-groups': () => jsonResponse({ groups: [], total: 0 }),
      'GET /projects': () => jsonResponse({ projects: [], total: 0 }),
    })
    const list = await view.findByRole('list', { name: 'Projects' })
    const add = await within(list).findByRole('link', {
      name: 'Create service',
    })
    expect(add.getAttribute('href')).toBe('/projects/new')
  })

  test('a service past the page of services read still lists its views', async () => {
    // A large server: the service open is not in the page the tree reads.
    const { view, api } = renderAt('/projects/late-service/deployments', {
      ...projects,
      'GET /projects': () =>
        jsonResponse({
          projects: [service(4, 'web'), service(5, 'api'), service(9, 'blog')],
          total: 240,
        }),
      'GET /projects/by-slug/late-service': () =>
        jsonResponse(service(300, 'late-service')),
    })
    const row = await view.findByRole('link', { name: /late-service/ })
    expect(row.getAttribute('href')).toBe('/projects/late-service')
    const views = await view.findByRole('list', {
      name: 'late-service views',
    })
    expect(
      within(views).getByRole('link', { name: 'Deployments' })
    ).toBeTruthy()
    expect(api.called('GET', '/projects/by-slug/late-service')).toBe(true)
  })

  test('a private worker failing its health checks is shown in trouble', async () => {
    const { view } = renderAt('/projects', {
      ...projects,
      'GET /monitors-health/projects': () =>
        jsonResponse({
          projects: {
            '9': {
              project_id: 9,
              status: 'no_address',
              runtime_status: 'unhealthy',
            },
          },
        }),
    })
    const blog = await view.findByRole('link', { name: /blog/ })
    await waitFor(() => expect(blog.textContent).toContain('Unhealthy'))
  })

  test('a service the page lists is not asked for again', async () => {
    const { view, api } = renderAt('/projects/blog/deployments', projects)
    await view.findByRole('list', { name: 'blog views' })
    expect(api.called('GET', '/projects/by-slug/blog')).toBe(false)
  })

  test("the tree's failed reads are its rows' to show, not the page's", async () => {
    // A Reader is refused the nodes, and the projects list fails: each is
    // the tree's to say, so the page's failure banner reports neither.
    const { view, api, client } = renderAt('/projects', {
      ...projects,
      'GET /projects': () => jsonResponse({ title: 'Boom' }, 500),
      'GET /internal/nodes': () => jsonResponse({ title: 'Forbidden' }, 403),
    })
    const list = await view.findByRole('list', { name: 'Projects' })
    expect(await within(list).findByText('Could not be loaded')).toBeTruthy()
    await waitFor(() => expect(api.called('GET', '/internal/nodes')).toBe(true))
    await waitFor(() =>
      expect(
        client
          .getQueryCache()
          .getAll()
          .filter((query) => query.state.status === 'error').length
      ).toBeGreaterThanOrEqual(2)
    )
    expect(collectRouteQueryFailures(client)).toEqual([])
  })

  test('a list that could not be loaded says so and offers a retry', async () => {
    let fail = true
    const { view } = renderAt('/projects', {
      ...projects,
      'GET /projects': () =>
        fail
          ? jsonResponse({ title: 'Boom' }, 500)
          : jsonResponse({ projects: [], total: 0 }),
    })
    const list = await view.findByRole('list', { name: 'Projects' })
    expect(await within(list).findByText('Could not be loaded')).toBeTruthy()
    fail = false
    fireEvent.click(within(list).getByRole('button', { name: 'Retry' }))
    expect(await within(list).findByRole('link', { name: /Shop/ })).toBeTruthy()
  })
})
