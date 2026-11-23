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

describe('service inside a project', () => {
  test('keeps the project sidebar and opens the service as a level in it', () => {
    mockApi({})
    const service = {
      ...api,
      source_type: 'docker_image',
    } as unknown as ProjectResponse
    const view = render(
      <Shell at="/projects/api/runtime">
        <GroupNav
          group={group}
          activeService={service}
          serviceRoute="runtime"
        />
      </Shell>
    )
    const links = hrefs(view.container)
    // Still the project's sidebar…
    expect(links).toContain('/p/storefront/databases')
    expect(links).toContain('/projects/web')
    // …with the open service's sections nested under it, and only its.
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
    // No project-level page is marked current while a service is open.
    const projectDatabases = view.container.querySelector(
      'a[href="/p/storefront/databases"]'
    )
    expect(projectDatabases?.hasAttribute('aria-current')).toBe(false)
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
    expect(await view.findByRole('list', { name: 'api sections' }))
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
