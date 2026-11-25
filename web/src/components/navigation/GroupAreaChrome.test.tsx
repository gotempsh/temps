// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { PageHeader } from '@/components/layout/PageContainer'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { GroupAreaChrome } from './GroupAreaChrome'

installBrowserWindow()
afterEach(() => {
  cleanup()
  window.localStorage.clear()
})

const deployed = (id: number, name: string) => ({
  id,
  name,
  slug: name,
  source_type: 'git',
})
const monitored = (id: number, name: string) => ({
  id,
  name,
  slug: name,
  source_type: 'external',
})

function renderAt(
  route: string,
  projects: ReturnType<typeof deployed>[] = [deployed(4, 'web')]
) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[`/p/storefront/${route}`]}>
        <GroupAreaChrome
          group={{ id: 3, name: 'Storefront', slug: 'storefront', projects }}
          route={route}
        >
          <PageHeader title="The page" />
        </GroupAreaChrome>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const titles = (nav: HTMLElement) =>
  [...nav.querySelectorAll('a')].map((a) => a.textContent)

const VIEWS = 'Storefront views'

test("the project's views are the tabs of its page", () => {
  mockApi({})
  const view = renderAt('logs')
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(
    [...nav.querySelectorAll('a')].map((a) => [
      a.textContent,
      a.getAttribute('href'),
    ])
  ).toEqual([
    ['Overview', '/p/storefront'],
    ['Deployments', '/p/storefront/deployments'],
    ['Logs', '/p/storefront/logs'],
    ['Errors', '/p/storefront/errors'],
    ['Traces', '/p/storefront/traces'],
    ['Analytics', '/p/storefront/analytics'],
    ['Domains', '/p/storefront/domains'],
    ['Databases', '/p/storefront/databases'],
    ['Shared variables', '/p/storefront/variables'],
    ['Settings', '/p/storefront/settings'],
  ])
  expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe('Logs')
  // No parent groups them by kind of data.
  for (const parent of ['Observe', 'Resources', 'Configure'])
    expect(view.queryByText(parent)).toBeNull()
})

test('a view no service uses yet is offered under Add, not as a tab', async () => {
  mockApi({
    'GET /otel/has-traces/4': () => jsonResponse({ has_traces: false }),
    'GET /projects/4/has-events': () => jsonResponse({ has_events: true }),
    'GET /projects/4/custom-domains': () => jsonResponse({ domains: [] }),
    'GET /projects/4/services': () => jsonResponse([{ id: 1 }]),
  })
  const view = renderAt('logs')
  const nav = view.getByRole('navigation', { name: VIEWS })
  await waitFor(() => expect(titles(nav)).not.toContain('Traces'))
  expect(titles(nav)).not.toContain('Domains')
  expect(titles(nav)).toContain('Analytics')
  expect(titles(nav)).toContain('Databases')

  fireEvent.pointerDown(view.getByRole('button', { name: 'Add' }), {
    button: 0,
    ctrlKey: false,
  })
  const menu = await view.findByRole('menu')
  expect(
    [...menu.querySelectorAll('a')].map((a) => a.getAttribute('href'))
  ).toEqual(['/p/storefront/traces', '/p/storefront/domains'])
  expect(menu.textContent).toContain('No service sends traces yet.')
})

test('a view none of its services can have is not offered at all', () => {
  mockApi({})
  const view = renderAt('logs', [monitored(9, 'legacy')])
  const nav = view.getByRole('navigation', { name: VIEWS })
  expect(titles(nav)).toEqual([
    'Overview',
    'Logs',
    'Errors',
    'Traces',
    'Analytics',
    'Settings',
  ])
  expect(view.queryByRole('button', { name: 'Add' })).toBeNull()
})

test('Databases and Backups are one view with two pages', () => {
  mockApi({})
  const view = renderAt('backups')
  expect(
    view
      .getByRole('navigation', { name: VIEWS })
      .querySelector('[aria-current="page"]')?.textContent
  ).toBe('Databases')
  const pages = view.getByRole('navigation', { name: 'Databases pages' })
  expect(
    [...pages.querySelectorAll('a')].map((a) => [
      a.textContent,
      a.getAttribute('href'),
    ])
  ).toEqual([
    ['Databases', '/p/storefront/databases'],
    ['Backups', '/p/storefront/backups'],
  ])
  expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Backups'
  )
})

test('a record below a view stands alone, without the tabs', () => {
  mockApi({})
  const view = renderAt('backups/3')
  expect(view.container.querySelector('nav')).toBeNull()
})

test('the Overview names the project; another view is named by its tab', () => {
  mockApi({})
  const overview = renderAt('')
  expect(overview.getByRole('heading', { level: 1 }).className).not.toContain(
    'sr-only'
  )
  cleanup()
  const logs = renderAt('logs')
  // Kept for assistive technology, not printed twice.
  expect(logs.getByRole('heading', { level: 1 }).className).toContain('sr-only')
})
