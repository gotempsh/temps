// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { SidebarProvider, useSidebar } from '@/components/ui/sidebar'
import { projectLocationFor } from '@/lib/project-navigation'
import { installBrowserWindow } from '@/test/dom'
import { ProjectSectionTabs } from './ProjectSectionTabs'

installBrowserWindow()
type HappyDomWindow = { happyDOM?: { setViewport(viewport: { width: number }): void } }
// `window.innerWidth` is happy-dom's accessor on the global every test file
// shares. Resize through happy-dom, as the other viewport tests do; when that
// is unavailable, shadow the accessor and put the original descriptor back
// after each test. Replacing it with a fixed value would stop later files'
// `setViewport` calls from changing the width.
const desktopWidth = window.innerWidth
const originalInnerWidth = Object.getOwnPropertyDescriptor(window, 'innerWidth')
function setWidth(width: number) {
  const happyDOM = (window as unknown as HappyDomWindow).happyDOM
  if (happyDOM) {
    happyDOM.setViewport({ width })
    return
  }
  Object.defineProperty(window, 'innerWidth', { value: width, configurable: true, writable: true })
}
function restoreWidth() {
  const happyDOM = (window as unknown as HappyDomWindow).happyDOM
  if (happyDOM) happyDOM.setViewport({ width: desktopWidth })
  if (originalInnerWidth) Object.defineProperty(window, 'innerWidth', originalInnerWidth)
  else Reflect.deleteProperty(window, 'innerWidth')
}
afterEach(() => {
  cleanup()
  restoreWidth()
  window.localStorage.clear()
})

function Drawer() {
  return <p data-testid="drawer">{String(useSidebar().openMobile)}</p>
}

function renderTabs(route: string, sourceType = 'git') {
  const project = { id: 7, slug: 'synthetic-api', source_type: sourceType }
  return render(
    <MemoryRouter initialEntries={[`/projects/synthetic-api/${route}`]}>
      <SidebarProvider>
        <ProjectSectionTabs
          project={project}
          location={projectLocationFor(project, route.split('?')[0])}
        />
        <Drawer />
      </SidebarProvider>
    </MemoryRouter>
  )
}

const titles = (nav: HTMLElement) =>
  [...nav.querySelectorAll('a')].map((a) => a.textContent)

const open = (trigger: HTMLElement) =>
  fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })

test("a service's page has one row of tabs: the pages of the view shown", () => {
  const view = renderTabs('environments')
  // The views themselves are in the sidebar, not a strip above this one.
  expect(view.getAllByRole('navigation')).toHaveLength(1)
  const pages = view.getByRole('navigation', { name: 'Deployments pages' })
  expect(titles(pages)).toEqual(['Deployments', 'Environments', 'Build'])
  expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Environments'
  )
})

test('the Overview holds what says whether the service is healthy, and what it owns', async () => {
  const view = renderTabs('metrics/dashboards')
  expect(view.getAllByRole('navigation')).toHaveLength(1)
  const pages = view.getByRole('navigation', { name: 'Overview pages' })
  expect(
    [...pages.querySelectorAll('[data-tab]')].map((tab) => tab.textContent)
  ).toEqual([
    'Overview',
    'Activity',
    'MetricsDashboards',
    'Uptime',
    'Alerts',
    'Domains',
    'Databases',
  ])
  // A held section of several pages opens a menu, marked on any of them.
  const metrics = view.getByRole('button', { name: /^Metrics/ })
  expect(metrics.getAttribute('aria-current')).toBe('true')
  // The quick tour points at it here.
  expect(metrics.getAttribute('data-tour')).toBe('metrics')
  open(metrics)
  const menu = await view.findByRole('menu')
  expect(
    [...menu.querySelectorAll('a')].map((a) => [
      a.textContent,
      a.getAttribute('href'),
    ])
  ).toEqual([
    ['Metrics', '/projects/synthetic-api/metrics/explore'],
    ['Dashboards', '/projects/synthetic-api/metrics/dashboards'],
  ])
  expect(menu.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Dashboards'
  )
  cleanup()
  // One of a single page is a plain tab.
  const domains = renderTabs('domains')
  const link = domains.getByRole('link', { name: 'Domains' })
  expect(link.getAttribute('aria-current')).toBe('page')
  expect(link.getAttribute('href')).toBe('/projects/synthetic-api/domains')
})

test('Logs holds the traces, as what happened', () => {
  const view = renderTabs('ai-gateway')
  const pages = view.getByRole('navigation', { name: 'Logs pages' })
  expect(
    [...pages.querySelectorAll('[data-tab]')].map((tab) => tab.textContent)
  ).toEqual(['Application', 'Requests', 'OpenTelemetry', 'TracesAI traces'])
  expect(
    view.getByRole('button', { name: /^Traces/ }).getAttribute('data-tour')
  ).toBe('traces')
})

test('security, automation and the service settings are the pages of Settings', () => {
  const view = renderTabs('settings/edge-rules')
  expect(view.getAllByRole('navigation')).toHaveLength(1)
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
  const view = renderTabs('settings/edge-rules/new')
  const pages = view.getByRole('navigation', { name: 'Settings pages' })
  expect(pages.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Edge rules'
  )
})

test('a page that holds several is a tab that opens a menu, not a second row', async () => {
  const view = renderTabs('errors/inbound-filters')
  expect(view.getAllByRole('navigation')).toHaveLength(1)
  const pages = view.getByRole('navigation', { name: 'Errors pages' })
  expect(titles(pages)).toEqual(['Issues', 'Feedback', 'Releases'])
  // The tab is marked on any of its pages, and says which one is shown.
  const settings = view.getByRole('button', { name: /^Settings/ })
  expect(settings.getAttribute('aria-current')).toBe('true')
  expect(settings.textContent).toBe('SettingsInbound filters')

  open(settings)
  const menu = await view.findByRole('menu')
  expect(
    [...menu.querySelectorAll('a')].map((a) => [
      a.textContent,
      a.getAttribute('href'),
    ])
  ).toEqual([
    ['Source maps', '/projects/synthetic-api/errors/source-maps'],
    ['Inbound filters', '/projects/synthetic-api/errors/inbound-filters'],
    ['Attachments', '/projects/synthetic-api/errors/attachments'],
    ['Ownership', '/projects/synthetic-api/errors/ownership'],
    ['SDK health', '/projects/synthetic-api/client-outcomes'],
    ['Setup', '/projects/synthetic-api/errors/setup'],
  ])
  expect(menu.querySelector('[aria-current="page"]')?.textContent).toBe(
    'Inbound filters'
  )
})

test('a tab that opens a menu is not marked while another tab is shown', () => {
  const view = renderTabs('errors')
  const settings = view.getByRole('button', { name: /^Settings/ })
  expect(settings.getAttribute('aria-current')).toBeNull()
  expect(settings.textContent).toBe('Settings')
})

test('a link keeps the environment chosen in the bar', () => {
  const view = renderTabs('deployments?env=staging')
  expect(
    view.getByRole('link', { name: 'Environments' }).getAttribute('href')
  ).toBe('/projects/synthetic-api/environments?env=staging')
})

test('on a desktop the sidebar names the view, so the page adds no button', () => {
  const view = renderTabs('settings/general')
  expect(view.queryByRole('button', { name: /Show all views/ })).toBeNull()
})

test('on a phone a button names the view and opens the sidebar on its views', () => {
  setWidth(390)
  const view = renderTabs('settings/general')
  const button = view.getByRole('button', {
    name: 'Settings. Show all views',
  })
  expect(view.getByTestId('drawer').textContent).toBe('false')
  fireEvent.click(button)
  expect(view.getByTestId('drawer').textContent).toBe('true')
})

test('"Add hosting" belongs to no view: no tabs, and the button says so', () => {
  setWidth(390)
  const view = renderTabs('hosting', 'external')
  expect(view.queryByRole('navigation')).toBeNull()
  expect(
    view.getByRole('button', { name: 'Add hosting. Show all views' })
  ).toBeTruthy()
})
