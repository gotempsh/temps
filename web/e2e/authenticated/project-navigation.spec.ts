// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test, type Page } from '@playwright/test'
import type { ProjectResponse } from '../../src/api/client'

async function hostedProjectRoot(page: Page) {
  const { projects } = await (await page.request.get('/api/projects')).json()
  expect(projects.length).toBeGreaterThan(0)
  const project = projects.find(
    (project: ProjectResponse) =>
      project.hosting === 'hosted' && project.source_type !== 'external'
  )
  expect(project, 'Hosted navigation needs a hosted project').toBeDefined()
  return `/projects/${project.slug}`
}

// Each section's pages, in order. Naming them (rather than only counting) is
// what catches a page being added, dropped or renamed; `toHaveText` with an
// array also pins the count.
const SECTION_TABS = [
  ['Overview', ['Overview', 'Activity']],
  ['Databases', ['Databases', 'Key-value store', 'Blob storage']],
  ['Logs', ['Application', 'Requests', 'Telemetry']],
  [
    'Errors',
    [
      'Issues',
      'Alerts',
      'Feedback',
      'Releases',
      'Source maps',
      'SDK health',
      'Setup',
    ],
  ],
  ['Traces', ['Traces', 'AI traces']],
  [
    'Analytics',
    [
      'Overview',
      'Visitors',
      'Live',
      'Globe',
      'Journey',
      'Sessions',
      'Pages',
      'Funnels',
      'Activity report',
      'Web performance',
      'AI visitors',
      'AI crawlers',
      'API traffic',
      'Revenue',
      'Traffic filters',
    ],
  ],
  ['Monitoring', ['Metrics', 'Dashboards', 'Alerts', 'Uptime', 'Cron jobs']],
  [
    'Variables & secrets',
    ['Environment variables', 'Secrets', 'Deployment tokens'],
  ],
  ['Security & access', ['Protection', 'Access', 'Scans']],
  ['Automation & integrations', ['Automation', 'Integrations', 'Telemetry']],
] as const

// Errors sends a project that has never reported one to its Setup tab, so
// its menu entry may land there instead of on Issues.
const OPENS_ON: Partial<Record<string, readonly string[]>> = {
  Errors: ['Issues', 'Setup'],
}

test('the project menu groups its sections and each section lists its pages as tabs', async ({
  page,
}) => {
  const root = await hostedProjectRoot(page)
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(`${root}/settings`)
  await expect(page).toHaveURL(/settings\/general$/)

  const menu = (name: string) => page.getByRole('list', { name, exact: true })
  await expect(menu('Project').getByRole('link')).toHaveText(['Overview'])
  await expect(menu('Ship').getByRole('link')).toHaveText([
    'Deployments',
    'Environments',
    'Domains',
    'Databases',
  ])
  await expect(menu('Observe').getByRole('link')).toHaveText([
    'Logs',
    'Errors',
    'Traces',
    'Analytics',
    'Monitoring',
  ])
  await expect(menu('Configure').getByRole('link')).toHaveText([
    'Build & deploy',
    'Variables & secrets',
    'Security & access',
    'Automation & integrations',
    'General',
  ])
  await expect(
    menu('Configure').getByRole('link', { name: 'General', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  // General is a single page: no row of tabs above it.
  await expect(
    page.getByRole('navigation', { name: 'General pages' })
  ).toHaveCount(0)

  const sidebar = page.locator('[data-sidebar="sidebar"]')
  for (const [section, tabs] of SECTION_TABS) {
    const link = sidebar.getByRole('link', { name: section, exact: true })
    await link.click()
    await expect(link).toHaveAttribute('aria-current', 'page')
    const nav = page.getByRole('navigation', { name: `${section} pages` })
    await expect(nav.getByRole('link')).toHaveText([...tabs])
    // The menu entry opens the section's first page.
    const current = nav.locator('a[aria-current="page"]')
    await expect(current).toHaveCount(1)
    await expect(current).toHaveText(
      new RegExp(`^(${(OPENS_ON[section] ?? [tabs[0]]).join('|')})$`)
    )
  }
  await page.screenshot({
    path: '/tmp/temps-project-navigation.png',
    fullPage: true,
  })

  // A tab other than the first keeps its section selected in the menu.
  await sidebar.getByRole('link', { name: 'Monitoring', exact: true }).click()
  const monitoring = page.getByRole('navigation', { name: 'Monitoring pages' })
  await monitoring.getByRole('link', { name: 'Cron jobs', exact: true }).click()
  await expect(page).toHaveURL(/\/cron-jobs$/)
  await expect(
    monitoring.getByRole('link', { name: 'Cron jobs', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await expect(
    sidebar.getByRole('link', { name: 'Monitoring', exact: true })
  ).toHaveAttribute('aria-current', 'page')

  await sidebar
    .getByRole('link', { name: 'Build & deploy', exact: true })
    .click()
  await expect(page).toHaveURL(/\/build-deploy$/)
  await expect(
    page.getByRole('navigation', { name: 'Build & deploy pages' })
  ).toHaveCount(0)
  expect(errors).toEqual([])
})

test('legacy build links and the mobile project menu stay usable', async ({
  page,
}) => {
  const root = await hostedProjectRoot(page)
  await page.setViewportSize({ width: 390, height: 844 })
  // The old standalone build page redirects to its section of Build & deploy
  // and scrolls it into view; every section is always open.
  await page.goto(`${root}/settings/build?tab=previews`)
  await expect(page).toHaveURL(/settings\/delivery\?section=previews$/)
  await expect(page.locator('#settings-section-previews')).toBeInViewport()

  await page.getByRole('button', { name: 'Toggle sidebar' }).first().click()
  const menu = page.getByRole('dialog')
  await menu
    .getByRole('link', { name: 'Automation & integrations', exact: true })
    .click()
  await expect(page).toHaveURL(/settings\/automation$/)
  await expect(menu).not.toBeVisible()
  await expect(
    page
      .getByRole('navigation', { name: 'Automation & integrations pages' })
      .getByRole('link', { name: 'Automation', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  // The tabs scroll inside their row; the page itself never scrolls sideways.
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  ).toBe(true)
})
