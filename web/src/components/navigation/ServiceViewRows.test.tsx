// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { SidebarProvider } from '@/components/ui/sidebar'
import { setProjectTourActive } from '@/lib/project-tour'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CompactServiceViews, ServiceViewRows } from './ServiceViewRows'

installBrowserWindow()
afterEach(() => {
  cleanup()
  setProjectTourActive(false)
  window.localStorage.clear()
})

/** What the service holds: page views, and its custom domains. */
function mockUse(use: {
  analytics?: boolean
  domains?: { domain: string; status: string }[]
}) {
  return mockApi({
    'GET /projects/7/has-events': () =>
      jsonResponse({ has_events: use.analytics ?? false }),
    'GET /projects/7/custom-domains': () =>
      jsonResponse({
        domains: (use.domains ?? []).map((domain, id) => ({ id, ...domain })),
      }),
  })
}

function renderRows(route: string, sourceType = 'git') {
  const navigated: string[] = []
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[`/projects/synthetic-api/${route}`]}>
        <SidebarProvider>
          <ul aria-label="synthetic-api views">
            <ServiceViewRows
              service={{
                id: 7,
                slug: 'synthetic-api',
                source_type: sourceType,
              }}
              onNavigate={() => navigated.push('navigated')}
            />
          </ul>
        </SidebarProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, navigated, list: view.getByRole('list') }
}

/** The view rows; a service's domains are listed after them. */
const titles = (list: HTMLElement) =>
  [...list.querySelectorAll('a[data-view]')].map((a) => a.textContent)

test("a service's views are the everyday rows under it, Settings last", () => {
  mockUse({ analytics: true })
  const { list } = renderRows('deployments')
  expect(titles(list)).toEqual([
    'Overview',
    'Deployments',
    'Logs',
    'Errors',
    'Variables',
    'Analytics',
    'Settings',
  ])
  expect(list.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Deployments'
  )
})

test('a page of a view held by another marks the view holding it', () => {
  mockApi({})
  for (const [route, row] of [
    ['metrics/dashboards', 'Overview'],
    ['monitors', 'Overview'],
    ['domains', 'Overview'],
    ['traces', 'Logs'],
  ]) {
    const { list } = renderRows(route)
    const current = list.querySelector('[aria-current]')
    expect(current?.textContent).toBe(row)
    expect(current?.getAttribute('aria-current')).toBe('true')
    cleanup()
  }
})

test('a page below a view keeps the view marked, as the place it is in', () => {
  mockApi({})
  const { list } = renderRows('errors/42')
  const errors = list.querySelector('[aria-current]')
  expect(errors?.textContent).toBe('Errors')
  expect(errors?.getAttribute('aria-current')).toBe('true')
})

test('Analytics is a row once the service receives page views; until then it is under Add', async () => {
  mockUse({ analytics: false })
  const { view, list, navigated } = renderRows('deployments')
  await waitFor(() => expect(titles(list)).not.toContain('Analytics'))
  fireEvent.pointerDown(view.getByRole('button', { name: 'Add a view' }), {
    button: 0,
    ctrlKey: false,
  })
  const menu = await view.findByRole('menu')
  const links = [...menu.querySelectorAll('a')]
  expect(links.map((a) => a.getAttribute('href'))).toEqual([
    '/projects/synthetic-api/analytics',
  ])
  // It says what is missing.
  expect(menu.textContent).toContain('No page view received yet.')
  // Choosing it closes a phone's drawer like any other row.
  fireEvent.click(links[0])
  expect(navigated).toEqual(['navigated'])
})

test('nothing is hidden on a guess: unanswered, every view is a row', () => {
  mockApi({})
  const { view, list } = renderRows('deployments')
  expect(titles(list)).toContain('Analytics')
  expect(view.queryByRole('button', { name: 'Add a view' })).toBeNull()
})

test('the view shown keeps its row even when it holds nothing', async () => {
  const api = mockUse({ analytics: false })
  const { list } = renderRows('analytics')
  await waitFor(() =>
    expect(api.called('GET', '/projects/7/has-events')).toBe(true)
  )
  expect(list.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Analytics'
  )
})

test('a service with custom domains has one Domains row, last, saying how many are in trouble', async () => {
  mockUse({
    domains: [
      { domain: 'shop.example.test', status: 'active' },
      { domain: 'www.shop.example.test', status: 'pending' },
      { domain: 'old.example.test', status: 'pending_dns' },
    ],
  })
  const { list } = renderRows('deployments')
  await waitFor(() => expect(titles(list)).toContain('Domains2 pending'))
  // One row, whatever the number of domains, after the views; no
  // hostname that would not fit.
  expect(titles(list)[titles(list).length - 1]).toBe('Domains2 pending')
  expect(list.textContent).not.toContain('example.test')
  expect(
    list.querySelector('a[data-view="domains"]')?.getAttribute('href')
  ).toBe('/projects/synthetic-api/domains')
})

test('the Domains row keeps the environment chosen in the bar', async () => {
  mockUse({ domains: [{ domain: 'shop.example.test', status: 'active' }] })
  const { list } = renderRows('deployments?env=staging')
  await waitFor(() =>
    expect(
      list.querySelector('a[data-view="domains"]')?.getAttribute('href')
    ).toBe('/projects/synthetic-api/domains?env=staging')
  )
})

test('the Domains row carries the mark on its page, not the Overview', async () => {
  mockUse({ domains: [{ domain: 'shop.example.test', status: 'active' }] })
  const { list } = renderRows('domains')
  const row = await waitFor(() => {
    const found = list.querySelector('a[data-view="domains"]')
    expect(found).toBeTruthy()
    return found
  })
  expect(row?.getAttribute('aria-current')).toBe('page')
  expect(
    [...list.querySelectorAll('[aria-current]')].map((a) => a.textContent)
  ).toEqual(['DomainsActive'])
})

test('a service with no custom domain has no Domains row, and its page marks the Overview', async () => {
  const api = mockUse({ domains: [] })
  const { list } = renderRows('domains')
  await waitFor(() =>
    expect(api.called('GET', '/projects/7/custom-domains')).toBe(true)
  )
  expect(titles(list)).not.toContain('Domains')
  expect(list.querySelector('[aria-current]')?.textContent).toBe('Overview')
})

test('the quick tour points at the views, so each is a row while it runs', async () => {
  const api = mockUse({ analytics: false })
  setProjectTourActive(true)
  const { list } = renderRows('project')
  // Still a row once its answer says it holds nothing.
  await waitFor(() =>
    expect(api.called('GET', '/projects/7/has-events')).toBe(true)
  )
  expect(list.querySelector('[data-tour="analytics"]')?.textContent).toBe(
    'Analytics'
  )
})

test('folded to icons, the sidebar still lists the views, named by label', async () => {
  const api = mockApi({
    'GET /projects/by-slug/synthetic-api': () =>
      jsonResponse({ id: 7, slug: 'synthetic-api', source_type: 'git' }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/projects/synthetic-api/runtime']}>
        <SidebarProvider>
          <CompactServiceViews slug="synthetic-api" />
        </SidebarProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const logs = await view.findByRole('link', { name: 'Logs' })
  expect(logs.getAttribute('href')).toBe('/projects/synthetic-api/runtime')
  expect(logs.getAttribute('aria-current')).toBe('page')
  expect(view.getByRole('link', { name: 'Settings' })).toBeTruthy()
  expect(api.calls.length).toBeGreaterThan(0)
})
