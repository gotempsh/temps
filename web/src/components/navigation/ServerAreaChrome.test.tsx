// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
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

// A view with several pages is a button that opens a menu of them.
const titles = (nav: HTMLElement) =>
  [...nav.querySelectorAll('[data-tab]')].map((tab) => tab.textContent)

const current = (nav: HTMLElement) =>
  nav.querySelector('[aria-current]')?.textContent

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
      // One row, whatever the view.
      expect(view.getAllByRole('navigation')).toHaveLength(1)
      cleanup()
    }
  })

  test('a view with several pages is a tab that opens a menu, not a second row', async () => {
    const view = renderAt('/monitoring/rules')
    expect(view.getAllByRole('navigation')).toHaveLength(1)
    const alerts = view.getByRole('button', { name: /^Alerts/ })
    expect(alerts.getAttribute('aria-current')).toBe('true')
    // It says which of its pages is shown.
    expect(alerts.textContent).toBe('AlertsRules')
    fireEvent.pointerDown(alerts, { button: 0, ctrlKey: false })
    const menu = await view.findByRole('menu')
    expect([...menu.querySelectorAll('a')].map((a) => a.textContent)).toEqual([
      'Alarms',
      'Rules',
      'Events',
      'Channels',
      'Preferences',
    ])
    expect(menu.querySelector('[aria-current="page"]')?.textContent).toBe(
      'Rules'
    )
  })

  test('gathers a page that lives at an unrelated URL', () => {
    const view = renderAt('/settings/notifications')
    expect(
      current(view.getByRole('navigation', { name: 'Projects views' }))
    ).toBe('AlertsChannels')
    cleanup()
    const revenue = renderAt('/revenue')
    expect(
      current(revenue.getByRole('navigation', { name: 'Projects views' }))
    ).toBe('AnalyticsRevenue')
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

describe('the Server page carries every record of the server', () => {
  const VIEWS = ['Nodes', 'Databases', 'Domains']

  test('its views are its tabs, each a menu of its pages', async () => {
    for (const [path, current] of [
      ['/settings/nodes', 'Nodes'],
      ['/monitoring/server', 'NodesThis server'],
      ['/storage', 'Databases'],
      ['/backups', 'DatabasesBackups'],
      ['/domains', 'Domains'],
      ['/certificates', 'DomainsCertificates'],
    ]) {
      const view = renderAt(path)
      const nav = view.getByRole('navigation', { name: 'Server views' })
      // A tab with several pages also names the one shown.
      expect(titles(nav)).toEqual(
        VIEWS.map((title) => (current.startsWith(title) ? current : title))
      )
      expect(nav.querySelector('[aria-current]')?.textContent).toBe(current)
      // One row, and no Logs or Errors beside a database.
      expect(view.getAllByRole('navigation')).toHaveLength(1)
      cleanup()
    }
    const view = renderAt('/storage')
    fireEvent.pointerDown(view.getByRole('button', { name: /^Domains/ }), {
      button: 0,
      ctrlKey: false,
    })
    const menu = await view.findByRole('menu')
    expect(
      [...menu.querySelectorAll('a')].map((a) => [
        a.textContent,
        a.getAttribute('href'),
      ])
    ).toEqual([
      ['Domains', '/domains'],
      ['Certificates', '/certificates'],
      ['DNS providers', '/dns-providers'],
      ['Delivery', '/delivery-profiles'],
    ])
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

  test('every tab leads with its icon, on a phone too', () => {
    const view = renderAt('/storage')
    const nav = view.getByRole('navigation', { name: 'Server views' })
    for (const tab of nav.querySelectorAll('[data-tab]')) {
      const icon = tab.firstElementChild
      expect(icon?.tagName.toLowerCase()).toBe('svg')
      expect(icon?.getAttribute('aria-hidden')).toBe('true')
      // No width at which it is dropped.
      expect(icon?.getAttribute('class')).not.toContain('hidden')
    }
  })
})

describe('an area of Settings with several pages', () => {
  test('has them as its tabs, with the page shown selected', () => {
    const view = renderAt('/ai-gateway/usage')
    const nav = view.getByRole('navigation', { name: 'Providers pages' })
    expect(titles(nav)).toEqual([
      'Providers',
      'Usage',
      'Models',
      'Limits',
      'Activity',
    ])
    expect(current(nav)).toBe('Usage')
  })
})
