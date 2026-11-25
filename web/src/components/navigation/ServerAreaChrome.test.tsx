// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { PageHeader } from '@/components/layout/PageContainer'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { ServerAreaChrome } from './ServerAreaChrome'

installBrowserWindow()
afterEach(cleanup)

function renderAt(path: string) {
  mockApi({})
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[path]}>
        <ServerAreaChrome>
          <PageHeader title="The page" />
        </ServerAreaChrome>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const tabs = (root: HTMLElement) =>
  [...root.querySelectorAll('nav a')].map((a) => [
    a.textContent,
    a.getAttribute('href'),
  ])

const titles = (nav: HTMLElement) =>
  [...nav.querySelectorAll('a')].map((a) => a.textContent)

describe('the Projects page carries what is seen across every project', () => {
  const VIEWS = ['Projects', 'Logs', 'Errors', 'Traces', 'Analytics', 'Alerts']

  test('its views are its tabs, on the list and on each view', () => {
    for (const [path, current] of [
      ['/projects', 'Projects'],
      ['/logs', 'Logs'],
      ['/errors', 'Errors'],
      ['/traces', 'Traces'],
    ]) {
      const view = renderAt(path)
      const nav = view.getByRole('navigation', { name: 'Projects views' })
      expect(titles(nav)).toEqual(VIEWS)
      expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe(
        current
      )
      // A view with one page has no second row.
      expect(view.getAllByRole('navigation')).toHaveLength(1)
      cleanup()
    }
  })

  test('a view with several pages lists them in a second row', () => {
    const view = renderAt('/monitoring/rules')
    const nav = view.getByRole('navigation', { name: 'Projects views' })
    expect(nav.querySelector('[aria-current="page"]')?.textContent).toBe(
      'Alerts'
    )
    const pages = view.getByRole('navigation', { name: 'Alerts pages' })
    expect(titles(pages)).toEqual([
      'Alarms',
      'Rules',
      'Events',
      'Channels',
      'Preferences',
    ])
    expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
      'Rules'
    )
  })

  test('gathers a page that lives at an unrelated URL', () => {
    const view = renderAt('/settings/notifications')
    expect(
      view
        .getByRole('navigation', { name: 'Alerts pages' })
        .querySelector('[aria-current="page"]')?.textContent
    ).toBe('Channels')
    cleanup()
    const revenue = renderAt('/revenue')
    expect(
      revenue
        .getByRole('navigation', { name: 'Projects views' })
        .querySelector('[aria-current="page"]')?.textContent
    ).toBe('Analytics')
  })

  test('a form or a project below it stands alone, without the strip', () => {
    for (const path of ['/projects/new', '/p/shop', '/monitoring/rules/new']) {
      const view = renderAt(path)
      expect(view.container.querySelector('nav')).toBeNull()
      cleanup()
    }
  })

  test('the selected tab names the page, so its title is not shown twice', () => {
    const view = renderAt('/projects')
    // Kept for assistive technology, not printed.
    expect(view.getByRole('heading', { level: 1 }).className).toContain(
      'sr-only'
    )
  })
})

describe('the tab strip above a branch of the tree', () => {
  test('shows the branch pages as links, with the page shown selected', () => {
    const view = renderAt('/backups')
    expect(
      view.getByRole('navigation', { name: 'Databases pages' })
    ).toBeTruthy()
    expect(tabs(view.container)).toEqual([
      ['Databases', '/storage'],
      ['Backups', '/backups'],
    ])
    expect(
      view.container.querySelector('nav [aria-current="page"]')?.textContent
    ).toBe('Backups')
    // Data is not among them: no Logs or Errors beside a database.
    expect(
      view.queryByRole('navigation', { name: 'Projects views' })
    ).toBeNull()
  })

  test('the selected tab names the page, so its title is not shown twice', () => {
    const view = renderAt('/storage')
    expect(view.getByRole('heading', { level: 1 }).className).toContain(
      'sr-only'
    )
    cleanup()
    const plain = renderAt('/account')
    expect(plain.getByRole('heading', { level: 1 }).className).not.toContain(
      'sr-only'
    )
  })

  test('a record or a form stands alone, without the strip', () => {
    for (const path of ['/storage/create', '/domains/12', '/email']) {
      const view = renderAt(path)
      expect(view.container.querySelector('nav')).toBeNull()
      cleanup()
    }
  })

  test('gathers pages that live at unrelated URLs', () => {
    expect(tabs(renderAt('/monitoring/server').container)).toEqual([
      ['Nodes', '/settings/nodes'],
      ['This server', '/monitoring/server'],
    ])
  })

  test('tab icons sit before the label and drop on a phone', () => {
    const view = renderAt('/storage')
    const icon = view.getByRole('link', { name: 'Backups' }).firstElementChild
    expect(icon?.tagName.toLowerCase()).toBe('svg')
    expect(icon?.getAttribute('aria-hidden')).toBe('true')
    expect(icon?.getAttribute('class')).toContain('max-sm:hidden')
  })
})
