// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { projectLocationFor } from '@/lib/project-navigation'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectSectionTabs } from './ProjectSectionTabs'

installBrowserWindow()
afterEach(() => {
  cleanup()
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

function renderTabs(route: string, sourceType = 'git') {
  const project = { id: 7, slug: 'synthetic-api', source_type: sourceType }
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[`/projects/synthetic-api/${route}`]}>
        <ProjectSectionTabs
          project={project}
          location={projectLocationFor(project, route)}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const titles = (nav: HTMLElement) =>
  [...nav.querySelectorAll('a')].map((a) => a.textContent)

const VIEWS = 'synthetic-api views'

test("the service's views are the tabs of its page, Settings last", async () => {
  mockUse({
    traces: true,
    analytics: true,
    monitors: 1,
    domains: 1,
    databases: 1,
  })
  const view = renderTabs('deployments')
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(titles(nav)).toEqual([
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
  expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Deployments'
  )
  // No parent groups them: nothing named for a kind of data.
  for (const parent of ['Observe', 'Resources', 'Configure'])
    expect(view.queryByText(parent)).toBeNull()
})

test('a view that holds nothing is offered under Add, not as a tab', async () => {
  mockUse({ traces: false, analytics: true, domains: 0, databases: 2 })
  const view = renderTabs('deployments')
  const nav = view.getByRole('navigation', { name: VIEWS })
  await waitFor(() => expect(titles(nav)).not.toContain('Traces'))
  expect(titles(nav)).not.toContain('Domains')
  expect(titles(nav)).not.toContain('Uptime')
  expect(titles(nav)).toContain('Analytics')
  expect(titles(nav)).toContain('Databases')

  fireEvent.pointerDown(view.getByRole('button', { name: 'Add' }), {
    button: 0,
    ctrlKey: false,
  })
  const menu = await view.findByRole('menu')
  expect(
    [...menu.querySelectorAll('a')].map((a) => a.getAttribute('href'))
  ).toEqual([
    '/projects/synthetic-api/traces',
    '/projects/synthetic-api/monitors',
    '/projects/synthetic-api/domains',
  ])
  // Each says what is missing.
  expect(menu.textContent).toContain('No trace received yet.')
  expect(menu.textContent).toContain('No custom domain yet.')
})

test('nothing is hidden on a guess: unanswered, every view is a tab', () => {
  mockApi({})
  const view = renderTabs('deployments')
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(titles(nav)).toContain('Traces')
  expect(titles(nav)).toContain('Domains')
  expect(view.queryByRole('button', { name: 'Add' })).toBeNull()
})

test('the view shown keeps its tab even when it holds nothing', async () => {
  mockUse({ traces: false })
  const view = renderTabs('traces')
  const nav = view.getByRole('navigation', { name: VIEWS })
  await waitFor(() => expect(titles(nav)).not.toContain('Domains'))
  expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe('Traces')
})

test('a view with several pages lists them in a second row', () => {
  mockApi({})
  const view = renderTabs('environments')
  const pages = view.getByRole('navigation', { name: 'Deployments pages' })
  expect(titles(pages)).toEqual(['Deployments', 'Environments', 'Build'])
  expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Environments'
  )
})

test('a view with one page has no second row', () => {
  mockApi({})
  const view = renderTabs('domains')
  expect(view.getAllByRole('navigation')).toHaveLength(1)
})

test('security, automation and the service settings are one Settings tab', () => {
  mockApi({})
  const view = renderTabs('settings/edge-rules')
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(titles(nav)).not.toContain('Security')
  expect(titles(nav)).not.toContain('Automation')
  const settings = nav.querySelector('[aria-current="page"]')
  expect(settings?.textContent).toBe('Settings')
  // It opens on the service's own settings.
  expect(settings?.getAttribute('href')).toBe(
    '/projects/synthetic-api/settings/general'
  )
  const pages = view.getByRole('navigation', { name: 'Settings pages' })
  expect(titles(pages)).toEqual([
    'General',
    'Data ingest',
    'Protection',
    'Edge rules',
    'Access',
    'Scans',
    'Automation',
    'Integrations',
  ])
  expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Edge rules'
  )
})

test('a rule opened for editing keeps the Edge rules page current', () => {
  mockApi({})
  const view = renderTabs('settings/edge-rules/new')
  const pages = view.getByRole('navigation', { name: 'Settings pages' })
  expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Edge rules'
  )
})

test('a page that holds several lists them in a third row', () => {
  mockApi({})
  const view = renderTabs('errors/inbound-filters')
  const second = view.getByRole('navigation', { name: 'Errors pages' })
  expect(titles(second)).toEqual(['Issues', 'Feedback', 'Releases', 'Settings'])
  // The page is current on any of its own, and opens on the first.
  const settings = second.querySelector('[aria-current="page"]')
  expect(settings?.textContent).toBe('Settings')
  expect(settings?.getAttribute('href')).toBe(
    '/projects/synthetic-api/errors/source-maps'
  )
  const third = view.getByRole('navigation', {
    name: 'Errors settings pages',
  })
  expect(titles(third)).toEqual([
    'Source maps',
    'Inbound filters',
    'Attachments',
    'Ownership',
    'SDK health',
    'Setup',
  ])
  expect(third.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Inbound filters'
  )
})

test('a link keeps the environment chosen in the bar', () => {
  mockApi({})
  const view = renderTabs('deployments?env=staging')
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(
    [...nav.querySelectorAll('a')].find((a) => a.textContent === 'Logs')?.href
  ).toContain('/projects/synthetic-api/runtime?env=staging')
})

test('a monitoring-only service has the views it registers, and Add hosting', () => {
  mockApi({})
  const view = renderTabs('errors', 'external')
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(titles(nav)).not.toContain('Deployments')
  expect(titles(nav)).not.toContain('Domains')
  expect(titles(nav)).toContain('Errors')
  expect(titles(nav)[titles(nav).length - 1]).toBe('Add hosting')
})

test('on a narrow strip, the current tab is scrolled into view', () => {
  mockApi({})
  // A 300px strip whose tabs are 100px apart: Workflows, the fourth, starts
  // at the edge.
  // Restored by descriptor: assigning the inherited method back would leave
  // an own copy on `HTMLElement` that hides later tests' fakes.
  const original = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    'getBoundingClientRect'
  )
  HTMLElement.prototype.getBoundingClientRect = function (this: HTMLElement) {
    if (this.tagName === 'NAV')
      return { left: 0, right: 300, width: 300 } as DOMRect
    const index = Array.from(this.parentElement?.children ?? []).indexOf(this)
    const left = index * 100 - (this.parentElement?.scrollLeft ?? 0)
    return { left, right: left + 80, width: 80 } as DOMRect
  }
  try {
    const view = renderTabs('durable-workflows')
    const nav = view.getByRole('navigation', { name: 'Uptime pages' })
    expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe(
      'Workflows'
    )
    // Centred: 300 - 0 - (300 - 80) / 2.
    expect(nav.scrollLeft).toBe(190)
  } finally {
    if (original)
      Object.defineProperty(
        HTMLElement.prototype,
        'getBoundingClientRect',
        original
      )
    else Reflect.deleteProperty(HTMLElement.prototype, 'getBoundingClientRect')
  }
})

test('a tab already in view leaves the strip where it is', () => {
  mockApi({})
  const view = renderTabs('metrics/explore')
  expect(
    view.getByRole('navigation', { name: 'Metrics pages' }).scrollLeft
  ).toBe(0)
})
