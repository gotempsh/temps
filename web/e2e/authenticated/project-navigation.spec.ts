// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test, type Page } from '@playwright/test'
import type { ProjectResponse } from '../../src/api/client'

async function hostedService(page: Page) {
  const { projects } = await (await page.request.get('/api/projects')).json()
  expect(projects.length).toBeGreaterThan(0)
  const project: ProjectResponse | undefined = projects.find(
    (project: ProjectResponse) =>
      project.hosting === 'hosted' && project.source_type !== 'external'
  )
  expect(project, 'Hosted navigation needs a hosted project').toBeDefined()
  const { name, slug } = project as ProjectResponse
  return { name, root: `/projects/${slug}` }
}

// The everyday views every hosted service has, in the sidebar's order.
// Analytics is a row once the service receives page views, and offered
// under "Add" until then; the occasional views are tabs of these.
const ALWAYS = [
  'Overview',
  'Deployments',
  'Logs',
  'Errors',
  'Variables',
  'Settings',
]

// Each view's pages, as the one row of tabs on its page. Naming them (rather
// than only counting) is what catches a page being added, dropped or renamed.
const VIEW_TABS = [
  // Is it healthy, and what it owns.
  [
    'Overview',
    [
      'Overview',
      'Activity',
      'Metrics',
      'Uptime',
      'Alerts',
      'Domains',
      'Databases',
    ],
  ],
  ['Deployments', ['Deployments', 'Environments', 'Build']],
  // What happened.
  ['Logs', ['Application', 'Requests', 'OpenTelemetry', 'Traces']],
  ['Variables', ['Variables', 'Secrets', 'Deploy tokens']],
  [
    'Settings',
    [
      'General',
      'Data ingest',
      'Protection',
      'Edge rules',
      'Access',
      'Scans',
      'Automation',
      'Integrations',
    ],
  ],
] as const

/** Every tab of a strip, a link or a tab that opens a menu. */
const tabsOf = (page: Page, view: string) =>
  page
    .getByRole('navigation', { name: `${view} pages`, exact: true })
    .locator('[data-tab]')

test("a service's views are listed under it in the sidebar, and its page has one row of tabs", async ({
  page,
}) => {
  const { name, root } = await hostedService(page)
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(`${root}/settings`)
  await expect(page).toHaveURL(/settings\/general$/)

  const views = page.getByRole('list', { name: `${name} views`, exact: true })
  // In order, whatever else is listed between them. Polled: the tree loads
  // its services on its own, after the page has redirected.
  await expect
    .poll(async () =>
      (await views.locator('a[data-view]').allTextContents()).filter((title) =>
        ALWAYS.includes(title)
      )
    )
    .toEqual(ALWAYS)
  await expect(
    views.getByRole('link', { name: 'Settings', exact: true })
  ).toHaveAttribute('aria-current', 'page')

  for (const [view, tabs] of VIEW_TABS) {
    const link = views.getByRole('link', { name: view, exact: true })
    await link.click()
    await expect(link).toHaveAttribute('aria-current', 'page')
    // One row of tabs on the page, never a second one under it.
    await expect(page.getByRole('navigation', { name: / pages$/ })).toHaveCount(
      1
    )
    const row = tabsOf(page, view)
    await expect(row).toHaveText([...tabs])
    // The view opens on its first page; every tab leads with its icon.
    await expect(row.first()).toHaveAttribute('aria-current', 'page')
    await expect(row.locator('> svg:first-child')).toHaveCount(tabs.length)
  }

  // Settings stay in the column of every other page: the tabs do not move.
  const settingsRow = await page
    .getByRole('navigation', { name: 'Settings pages', exact: true })
    .boundingBox()
  await views.getByRole('link', { name: 'Deployments', exact: true }).click()
  const deploymentsRow = await page
    .getByRole('navigation', { name: 'Deployments pages', exact: true })
    .boundingBox()
  expect(settingsRow?.x).toBe(deploymentsRow?.x)
  expect(settingsRow?.width).toBe(deploymentsRow?.width)

  // A page that holds several is a tab that opens a menu of them, and says
  // which one is shown.
  await page.goto(`${root}/errors/inbound-filters`)
  const settings = page
    .getByRole('navigation', { name: 'Errors pages', exact: true })
    .getByRole('button', { name: /^Settings/ })
  await expect(settings).toHaveAttribute('aria-current', 'true')
  await expect(settings).toContainText('Inbound filters')
  await settings.click()
  const menu = page.getByRole('menu')
  await expect(menu.getByRole('menuitem')).toHaveText([
    'Source maps',
    'Inbound filters',
    'Attachments',
    'Ownership',
    'SDK health',
    'Setup',
  ])
  await menu.getByRole('menuitem', { name: 'Ownership' }).click()
  await expect(page).toHaveURL(/\/errors\/ownership$/)
  await expect(settings).toContainText('Ownership')
  await expect(
    views.getByRole('link', { name: 'Errors', exact: true })
  ).toHaveAttribute('aria-current', 'true')
  expect(errors).toEqual([])
})

test('on a phone, the page names its view and opens the sidebar on the views', async ({
  page,
}) => {
  const { name, root } = await hostedService(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(`${root}/settings/automation`)

  // One row, cut to what fits: the tab shown stays, the rest are under More.
  const row = page.getByRole('navigation', {
    name: 'Settings pages',
    exact: true,
  })
  await expect(
    row.getByRole('link', { name: 'Automation', exact: true })
  ).toBeVisible()
  await expect(
    row.getByRole('button', { name: 'More Settings pages' })
  ).toBeVisible()
  // Nothing scrolls sideways, the strip included.
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  ).toBe(true)
  expect(await row.evaluate((nav) => nav.scrollWidth <= nav.clientWidth)).toBe(
    true
  )

  await page.getByRole('button', { name: 'Settings. Show all views' }).click()
  const drawer = page.getByRole('dialog')
  const views = drawer.getByRole('list', { name: `${name} views`, exact: true })
  // Marked as the view the page is in; its own page is General.
  await expect(
    views.getByRole('link', { name: 'Settings', exact: true })
  ).toHaveAttribute('aria-current', 'true')
  await views.getByRole('link', { name: 'Logs', exact: true }).click()
  await expect(page).toHaveURL(/\/runtime$/)
  await expect(drawer).not.toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Logs. Show all views' })
  ).toBeVisible()
  await expect(tabsOf(page, 'Logs').first()).toHaveAttribute(
    'aria-current',
    'page'
  )
})

test('a legacy build link still lands on its section of the delivery settings', async ({
  page,
}) => {
  const { root } = await hostedService(page)
  await page.setViewportSize({ width: 390, height: 844 })
  // The old standalone build page redirects to its section and scrolls it
  // into view; every section is always open.
  await page.goto(`${root}/settings/build?tab=previews`)
  await expect(page).toHaveURL(/settings\/delivery\?section=previews$/)
  await expect(page.locator('#settings-section-previews')).toBeInViewport()
})

test('the tree lists the projects and one Server row, whose page has every record of the server as tabs', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto('/backups')
  const tree = page.getByRole('list', { name: 'Your server', exact: true })
  await expect(tree.locator(':scope > li > a')).toHaveText([
    /^Projects/,
    /^Server/,
  ])
  await expect(tree.locator('[data-branch="server"] a')).toHaveAttribute(
    'aria-current',
    'true'
  )
  const strip = page.getByRole('navigation', {
    name: 'Server views',
    exact: true,
  })
  // One row; the tab shown names its page.
  await expect(
    page.getByRole('navigation', { name: / (pages|views)$/ })
  ).toHaveCount(1)
  await expect(strip.locator('[data-tab]')).toHaveText([
    'Nodes',
    'DatabasesBackups',
    'Domains',
  ])
  await strip.getByRole('button', { name: /^Domains/ }).click()
  await page.getByRole('menuitem', { name: 'Certificates' }).click()
  await expect(page).toHaveURL(/\/certificates$/)
  await expect(tree.locator('[data-branch="server"] a')).toHaveAttribute(
    'aria-current',
    'true'
  )
})
