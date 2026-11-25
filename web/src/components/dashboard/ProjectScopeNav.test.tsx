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
import type { ReactNode } from 'react'
import type {
  ProjectGroupResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import { SidebarProvider } from '@/components/ui/sidebar'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { GroupNav, ServiceScopeNav } from './ProjectScopeNav'
import { SERVICE_PAGES_KEY } from './service-pages-preference'
import { setProjectTourActive } from '@/lib/project-tour'

installBrowserWindow()
afterEach(cleanup)

const web = { id: 11, name: 'web', slug: 'web', preset: 'dockerfile' }
const api = { id: 12, name: 'api', slug: 'api', preset: 'dockerfile' }
const group = {
  id: 3,
  name: 'Storefront',
  slug: 'storefront',
  projects: [web, api],
} as unknown as ProjectGroupResponse

function Shell({ children, at = '/' }: { children: ReactNode; at?: string }) {
  return (
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[at]}>
        <SidebarProvider>{children}</SidebarProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const hrefs = (root: HTMLElement) =>
  [...root.querySelectorAll('a')].map((a) => a.getAttribute('href'))

describe('project scope', () => {
  test('the sidebar is the project: its pages, its services, a way out', () => {
    mockApi({})
    const view = render(
      <Shell at="/p/storefront/databases">
        <GroupNav group={group} activeSection="databases" />
      </Shell>
    )
    const links = hrefs(view.container)
    expect(links).toContain('/projects')
    expect(view.getByRole('button', { name: /Storefront: switch project/ }))
    for (const url of [
      '/p/storefront',
      '/p/storefront/deployments',
      '/p/storefront/logs',
      '/p/storefront/errors',
      '/p/storefront/analytics',
      '/p/storefront/domains',
      '/p/storefront/databases',
      '/p/storefront/backups',
      '/p/storefront/variables',
      '/p/storefront/settings',
      '/projects/web',
      '/projects/api',
    ])
      expect(links).toContain(url)
    // Server-wide administration is not part of a project's sidebar.
    for (const url of ['/storage', '/settings/nodes', '/settings/users'])
      expect(links).not.toContain(url)
    const current = view.container.querySelector('[aria-current="page"]')
    expect(current?.getAttribute('href')).toBe('/p/storefront/databases')
  })
})

describe('a project with different kinds of services', () => {
  const app = {
    ...web,
    hosting: 'hosted',
    source_type: 'git',
    project_type: 'server',
  }
  const site = {
    ...api,
    name: 'landing',
    slug: 'landing',
    hosting: 'hosted',
    source_type: 'static_files',
    project_type: 'static',
  }
  const monitored = {
    id: 13,
    name: 'legacy',
    slug: 'legacy',
    preset: 'dockerfile',
    hosting: 'external',
    source_type: 'external',
    project_type: 'server',
  }
  const withServices = (...projects: object[]) =>
    ({ ...group, projects }) as unknown as ProjectGroupResponse

  test('labels each service with its kind and says how many have a page', () => {
    mockApi({})
    const view = render(
      <Shell at="/p/storefront">
        <GroupNav
          group={withServices(app, site, monitored)}
          activeSection="overview"
        />
      </Shell>
    )
    const services = view.getByRole('list', { name: 'Services' })
    expect(within(services).getByRole('link', { name: 'legacy (Monitored)' }))
    expect(
      within(services).getByRole('link', { name: 'landing (Static site)' })
    )
    expect(within(services).getByRole('link', { name: 'web (App)' }))
    // Deployments: the app and the site, not the monitored service.
    expect(
      view.getByRole('link', { name: 'Deployments (2 of 3 services)' })
    ).toBeTruthy()
    // Databases: only the app can link one.
    expect(view.getByRole('link', { name: 'Databases (1 of 3 services)' }))
    // Errors: every kind sends them, so no count.
    expect(view.getByRole('link', { name: 'Errors' }))
  })

  test('hides pages none of its services can have', () => {
    mockApi({})
    const view = render(
      <Shell at="/p/storefront">
        <GroupNav group={withServices(monitored)} activeSection="overview" />
      </Shell>
    )
    const links = hrefs(view.container)
    for (const url of [
      '/p/storefront/deployments',
      '/p/storefront/domains',
      '/p/storefront/databases',
      '/p/storefront/backups',
      '/p/storefront/variables',
    ])
      expect(links).not.toContain(url)
    // A group left with no pages goes too.
    expect(view.queryByRole('list', { name: 'Resources' })).toBeNull()
    for (const url of [
      '/p/storefront',
      '/p/storefront/logs',
      '/p/storefront/errors',
      '/p/storefront/traces',
      '/p/storefront/analytics',
      '/p/storefront/settings',
    ])
      expect(links).toContain(url)
  })

  test('a project of one kind gets no kind labels or counts', () => {
    mockApi({})
    const view = render(
      <Shell at="/p/storefront">
        <GroupNav group={group} activeSection="overview" />
      </Shell>
    )
    expect(view.queryByText('App')).toBeNull()
    expect(view.getByRole('link', { name: 'Deployments' }))
  })
})

describe('service inside a project', () => {
  afterEach(() => window.localStorage.removeItem(SERVICE_PAGES_KEY))

  const service = {
    ...api,
    source_type: 'docker_image',
  } as unknown as ProjectResponse
  const renderService = () =>
    render(
      <Shell at="/projects/api/runtime">
        <GroupNav
          group={group}
          activeService={service}
          serviceRoute="runtime"
        />
      </Shell>
    )

  test("keeps the project sidebar, with the service's pages folded away", () => {
    mockApi({})
    const view = renderService()
    const links = hrefs(view.container)
    expect(links).toContain('/p/storefront/databases')
    expect(links).toContain('/projects/web')
    expect(links).toContain('/projects/api')
    // Its pages are listed only when the user asks for them.
    expect(view.queryByRole('list', { name: 'api sections' })).toBeNull()
    const toggle = view.getByRole('button', { name: 'Show api pages' })
    expect(toggle.getAttribute('aria-expanded')).toBe('false')
    // Only the open service can be unfolded.
    expect(view.queryByRole('button', { name: 'Show web pages' })).toBeNull()
    // No project-level page is marked current while a service is open.
    const projectDatabases = view.container.querySelector(
      'a[href="/p/storefront/databases"]'
    )
    expect(projectDatabases?.hasAttribute('aria-current')).toBe(false)
  })

  test("shows the service's pages while its quick tour points at them", () => {
    mockApi({})
    setProjectTourActive(true)
    try {
      const view = renderService()
      expect(view.getByRole('list', { name: 'api sections' })).toBeTruthy()
    } finally {
      setProjectTourActive(false)
    }
  })

  test('unfolds the open service as a level in it, and remembers that', () => {
    mockApi({})
    const view = renderService()
    fireEvent.click(view.getByRole('button', { name: 'Show api pages' }))
    const nested = view.getByRole('list', { name: 'api sections' })
    const nestedLinks = hrefs(nested)
    expect(nestedLinks).toContain('/projects/api/deployments')
    expect(nestedLinks).toContain('/projects/api/storage')
    expect(
      nestedLinks.every((href) => href?.startsWith('/projects/api/'))
    ).toBe(true)
    expect(view.queryByRole('list', { name: 'web sections' })).toBeNull()
    expect(
      within(nested)
        .getByRole('link', { name: 'Logs' })
        .getAttribute('aria-current')
    ).toBe('page')
    // The project's own pages are still there below it.
    expect(hrefs(view.container)).toContain('/p/storefront/databases')

    // The next service page opens the same way.
    cleanup()
    const next = renderService()
    expect(next.getByRole('list', { name: 'api sections' })).toBeTruthy()
    fireEvent.click(next.getByRole('button', { name: 'Hide api pages' }))
    expect(next.queryByRole('list', { name: 'api sections' })).toBeNull()
    expect(window.localStorage.getItem(SERVICE_PAGES_KEY)).toBeNull()
  })
})

describe('ServiceScopeNav', () => {
  test('a grouped service gets its project sidebar', async () => {
    mockApi({
      'GET /projects/by-slug/api': () =>
        jsonResponse({ ...api, source_type: 'docker_image' }),
      'GET /projects/12/group': () => jsonResponse({ group }),
    })
    const view = render(
      <Shell at="/projects/api">
        <ServiceScopeNav
          slug="api"
          route=""
          standalone={() => <p>standalone menu</p>}
        />
      </Shell>
    )
    expect(await view.findByRole('list', { name: 'Services' }))
    expect(view.getByRole('button', { name: 'Show api pages' }))
    expect(view.queryByText('standalone menu')).toBeNull()
  })

  test('a standalone service keeps its own menu', async () => {
    mockApi({
      'GET /projects/by-slug/solo': () =>
        jsonResponse({ id: 20, name: 'solo', slug: 'solo' }),
      'GET /projects/20/group': () => jsonResponse({ group: null }),
    })
    const view = render(
      <Shell at="/projects/solo">
        <ServiceScopeNav
          slug="solo"
          route=""
          standalone={(service) => <p>standalone menu for {service?.slug}</p>}
        />
      </Shell>
    )
    expect(await view.findByText('standalone menu for solo'))
  })

  test('a failed project lookup says so and retries, keeping the service menu', async () => {
    let lookups = 0
    mockApi({
      'GET /projects/by-slug/api': () =>
        jsonResponse({ ...api, source_type: 'docker_image' }),
      'GET /projects/12/group': () => {
        lookups += 1
        return jsonResponse({ title: 'Unavailable' }, 500)
      },
    })
    const view = render(
      <Shell at="/projects/api">
        <ServiceScopeNav
          slug="api"
          route=""
          standalone={(service, notice) => (
            <>
              {notice}
              <p>standalone menu for {service?.slug}</p>
            </>
          )}
        />
      </Shell>
    )
    const alert = await view.findByRole('alert')
    expect(alert.textContent).toContain('Could not load the project api')
    expect(view.getByText('standalone menu for api'))
    fireEvent.click(within(alert).getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(lookups).toBe(2))
  })

  test('the project switcher lists projects as links', async () => {
    mockApi({
      'GET /project-groups': () =>
        jsonResponse({ groups: [group], total: 1, page: 1, per_page: 100 }),
    })
    const view = render(
      <Shell at="/p/storefront">
        <GroupNav group={group} activeSection="overview" />
      </Shell>
    )
    fireEvent.pointerDown(
      view.getByRole('button', { name: /Storefront: switch project/ }),
      { button: 0, ctrlKey: false }
    )
    const menu = await view.findByRole('menu')
    await waitFor(() => {
      const items = within(menu).getAllByRole('menuitem')
      const links = items.map((item) => [
        item.tagName,
        item.getAttribute('href'),
      ])
      expect(links).toContainEqual(['A', '/p/storefront'])
      expect(links).toContainEqual(['A', '/projects'])
    })
  })
})
