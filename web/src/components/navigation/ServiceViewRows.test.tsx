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

/** What the service holds: which of its views are in use. */
function mockUse(use: {
  traces?: boolean
  analytics?: boolean
  monitors?: number
  domains?: number
  databases?: number
}) {
  const list = (count = 0) => Array.from({ length: count }, (_, id) => ({ id }))
  return mockApi({
    'GET /otel/has-traces/7': () =>
      jsonResponse({ has_traces: use.traces ?? false }),
    'GET /projects/7/has-events': () =>
      jsonResponse({ has_events: use.analytics ?? false }),
    'GET /projects/7/monitors': () => jsonResponse(list(use.monitors)),
    'GET /projects/7/custom-domains': () =>
      jsonResponse({ domains: list(use.domains) }),
    'GET /projects/7/services': () => jsonResponse(list(use.databases)),
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

const titles = (list: HTMLElement) =>
  [...list.querySelectorAll('a')].map((a) => a.textContent)

test("a service's views are the rows under it, Settings last", () => {
  mockUse({
    traces: true,
    analytics: true,
    monitors: 1,
    domains: 1,
    databases: 1,
  })
  const { list } = renderRows('deployments')
  expect(titles(list)).toEqual([
    'Overview',
    'Deployments',
    'Logs',
    'Errors',
    'Variables',
    'Traces',
    'Metrics',
    'Uptime',
    'Analytics',
    'Alerts',
    'Domains',
    'Databases',
    'Settings',
  ])
  expect(list.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Deployments'
  )
})

test('a page below a view keeps the view marked, as the place it is in', () => {
  mockApi({})
  const { list } = renderRows('errors/42')
  const errors = list.querySelector('[aria-current]')
  expect(errors?.textContent).toBe('Errors')
  expect(errors?.getAttribute('aria-current')).toBe('true')
})

test('a view that holds nothing is offered under Add, not as a row', async () => {
  mockUse({ traces: false, analytics: true, domains: 0, databases: 2 })
  const { view, list, navigated } = renderRows('deployments')
  await waitFor(() => expect(titles(list)).not.toContain('Traces'))
  expect(titles(list)).not.toContain('Domains')
  expect(titles(list)).not.toContain('Uptime')
  expect(titles(list)).toContain('Analytics')
  expect(titles(list)).toContain('Databases')

  fireEvent.pointerDown(view.getByRole('button', { name: 'Add a view' }), {
    button: 0,
    ctrlKey: false,
  })
  const menu = await view.findByRole('menu')
  const links = [...menu.querySelectorAll('a')]
  expect(links.map((a) => a.getAttribute('href'))).toEqual([
    '/projects/synthetic-api/traces',
    '/projects/synthetic-api/monitors',
    '/projects/synthetic-api/domains',
  ])
  // Each says what is missing.
  expect(menu.textContent).toContain('No trace received yet.')
  expect(menu.textContent).toContain('No custom domain yet.')
  // Choosing one closes a phone's drawer like any other row.
  fireEvent.click(links[0])
  expect(navigated).toEqual(['navigated'])
})

test('nothing is hidden on a guess: unanswered, every view is a row', () => {
  mockApi({})
  const { view, list } = renderRows('deployments')
  expect(titles(list)).toContain('Traces')
  expect(titles(list)).toContain('Domains')
  expect(view.queryByRole('button', { name: 'Add a view' })).toBeNull()
})

test('the view shown keeps its row even when it holds nothing', async () => {
  mockUse({ traces: false })
  const { list } = renderRows('traces')
  await waitFor(() => expect(titles(list)).not.toContain('Domains'))
  expect(list.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Traces'
  )
})

test('security, automation and the service settings are one Settings row', () => {
  mockApi({})
  const { list } = renderRows('settings/edge-rules')
  expect(titles(list)).not.toContain('Security')
  expect(titles(list)).not.toContain('Automation')
  const settings = list.querySelector('[aria-current]')
  expect(settings?.textContent).toBe('Settings')
  // It opens on the service's own settings.
  expect(settings?.getAttribute('href')).toBe(
    '/projects/synthetic-api/settings/general'
  )
})

test('a row keeps the environment chosen in the bar, and closes the drawer', () => {
  mockApi({})
  const { list, navigated } = renderRows('deployments?env=staging')
  const logs = [...list.querySelectorAll('a')].find(
    (a) => a.textContent === 'Logs'
  )
  expect(logs?.getAttribute('href')).toBe(
    '/projects/synthetic-api/runtime?env=staging'
  )
  if (logs) fireEvent.click(logs)
  expect(navigated).toEqual(['navigated'])
})

test('a monitoring-only service has the views it registers, and Add hosting', () => {
  mockApi({})
  const { list } = renderRows('errors', 'external')
  expect(titles(list)).not.toContain('Deployments')
  expect(titles(list)).not.toContain('Domains')
  expect(titles(list)).toContain('Errors')
  expect(titles(list)[titles(list).length - 1]).toBe('Add hosting')
})

test('"Add hosting" is marked on its own page, and no view is', () => {
  mockApi({})
  const { list } = renderRows('hosting', 'external')
  expect(
    [...list.querySelectorAll('[aria-current]')].map((a) => a.textContent)
  ).toEqual(['Add hosting'])
})

test('the quick tour points at the views, so each is a row while it runs', async () => {
  mockUse({ traces: false })
  setProjectTourActive(true)
  const { list } = renderRows('project')
  // Still a row once its answer says it holds nothing.
  await waitFor(() =>
    expect(list.querySelector('[data-tour="traces"]')).toBeTruthy()
  )
  expect(list.querySelector('[data-tour="traces"]')?.textContent).toBe('Traces')
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
