// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

async function mockEnvironmentMetrics(
  page: import('@playwright/test').Page,
  projectId: number,
  environmentId: number
) {
  await page.route(
    `**/api/projects/${projectId}/environments/${environmentId}/container-history**`,
    (route) =>
      route.fulfill({
        json: {
          containers: [
            {
              id: 1,
              container_id: 'e2e-container',
              container_name: 'web',
              deployment_id: 1,
              deployed_at: new Date().toISOString(),
              is_current: true,
            },
          ],
          total_count: 1,
        },
      })
  )
  await page.route(
    `**/api/projects/${projectId}/environments/${environmentId}/containers/*/metrics/history**`,
    (route) => {
      const now = Date.now()
      return route.fulfill({
        json: [0, 1, 2, 3].map((index) => ({
          time: new Date(now - (3 - index) * 60_000).toISOString(),
          value: index + 1,
        })),
      })
    }
  )
}

test('environment sidebar preserves the selected page when switching environments', async ({
  page,
}) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  const project =
    projects.find((item: { slug: string }) =>
      item.slug.startsWith('observability-starter')
    ) ?? projects[0]
  const environments = await (
    await page.request.get(`/api/projects/${project.id}/environments`)
  ).json()
  const production = environments[0]
  const staging = {
    ...production,
    id: 999991,
    name: 'staging',
    slug: 'staging',
    branch: 'staging',
    current_deployment_id: null,
  }
  await page.route(`**/api/projects/${project.id}/environments`, (route) =>
    route.fulfill({ json: [production, staging] })
  )
  await mockEnvironmentMetrics(page, project.id, production.id)
  await page.route(
    `**/api/projects/${project.id}/environments/${staging.id}`,
    (route) => route.fulfill({ json: staging })
  )
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(`/projects/${project.slug}/environments?view=metrics`)
  const sidebar = page.getByRole('complementary', {
    name: 'Environment navigation',
  })
  const nav = sidebar.getByRole('navigation', { name: 'Environment pages' })
  await expect(nav.getByRole('button')).toHaveText([
    'Containers',
    'Metrics',
    'Settings',
  ])
  await expect(
    nav.getByRole('button', { name: 'Metrics', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  // Real metrics must render SVG plots, not just correctly sized empty wrappers.
  const charts = page.locator('[data-chart] .recharts-surface')
  await expect(charts).toHaveCount(3)
  await expect(page.locator('.recharts-line-curve').first()).toBeVisible()
  for (const chart of await charts.all()) {
    const bounds = await chart.boundingBox()
    expect(bounds?.width).toBeGreaterThan(100)
    expect(bounds?.height).toBeGreaterThan(100)
  }
  await expect(
    sidebar.getByRole('button', {
      name: `Switch environment: ${production.name}`,
    })
  ).toHaveText(production.name)
  await sidebar
    .getByRole('button', { name: `Switch environment: ${production.name}` })
    .click()
  await page.getByRole('menuitemradio', { name: /staging/ }).click()
  await expect(page).toHaveURL(
    new RegExp(`view=metrics&environment=${staging.id}`)
  )
  await expect(
    sidebar.getByRole('button', { name: 'Switch environment: staging' })
  ).toBeVisible()
  await expect(
    nav.getByRole('button', { name: 'Metrics', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await nav.getByRole('button', { name: 'Settings', exact: true }).click()
  await page.reload()
  await expect(
    nav.getByRole('button', { name: 'Settings', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await sidebar
    .getByRole('button', { name: 'Switch environment: staging' })
    .click()
  await page
    .getByRole('menuitemradio', { name: new RegExp(production.name) })
    .click()
  await expect(page).toHaveURL(
    new RegExp(`view=settings&environment=${production.id}`)
  )
})

test('single-environment switcher offers creation and compact mobile navigation', async ({
  page,
}) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  const project =
    projects.find((item: { slug: string }) =>
      item.slug.startsWith('observability-starter')
    ) ?? projects[0]
  const environments = await (
    await page.request.get(`/api/projects/${project.id}/environments`)
  ).json()
  const environment = environments[0]
  await page.route(`**/api/projects/${project.id}/environments`, (route) =>
    route.fulfill({ json: [environment] })
  )
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(`/projects/${project.slug}/environments`)
  const sidebar = page.getByRole('complementary', {
    name: 'Environment navigation',
  })
  await sidebar
    .getByRole('button', { name: `Switch environment: ${environment.name}` })
    .click()
  await page.getByRole('menuitem', { name: 'Create environment' }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.press('Escape')
  await sidebar
    .getByRole('button', { name: 'Environment page: Containers' })
    .click()
  await page.getByRole('menuitem', { name: 'Metrics', exact: true }).click()
  await expect(
    sidebar.getByRole('button', { name: 'Environment page: Metrics' })
  ).toBeVisible()
  await expect(page).toHaveURL(/view=metrics/)
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  ).toBe(true)
  await page.screenshot({
    path: '/tmp/temps-environment-sidebar-mobile.png',
    fullPage: true,
  })
})

test('environment charts render with themed axis labels in dark mode', async ({
  page,
}) => {
  await page.addInitScript(() => localStorage.setItem('theme', 'dark'))
  await page.emulateMedia({ colorScheme: 'dark' })
  const { projects } = await (await page.request.get('/api/projects')).json()
  const project =
    projects.find((item: { slug: string }) =>
      item.slug.startsWith('observability-starter')
    ) ?? projects[0]
  await page.setViewportSize({ width: 1440, height: 1000 })
  const environments = await (
    await page.request.get(`/api/projects/${project.id}/environments`)
  ).json()
  await mockEnvironmentMetrics(page, project.id, environments[0].id)
  await page.goto(`/projects/${project.slug}/environments?view=metrics`)
  await expect(page.locator('html')).toHaveClass(/dark/)
  await expect(page.locator('[data-chart] .recharts-surface')).toHaveCount(3)
  const tick = page.locator('.recharts-cartesian-axis-tick-value').first()
  await expect(tick).toBeVisible()
  expect(
    await tick.evaluate((element) => getComputedStyle(element).fill)
  ).not.toBe('rgb(102, 102, 102)')
  await expect(page.locator('.recharts-line-curve').first()).toBeVisible()
  await page.screenshot({
    path: '/tmp/temps-charts-dark-desktop.png',
    fullPage: true,
  })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.locator('[data-chart] .recharts-surface')).toHaveCount(3)
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth
    )
  ).toBe(true)
  await page.screenshot({
    path: '/tmp/temps-charts-dark-mobile.png',
    fullPage: true,
  })
  await page.route('**/containers/*/metrics/history?*', (route) =>
    route.fulfill({ json: [{ time: new Date().toISOString(), value: 1 }] })
  )
  await page.getByRole('button', { name: '7d', exact: true }).click()
  await expect(page.locator('.recharts-line-dot')).toHaveCount(4)
  for (const point of await page.locator('.recharts-line-dot').all()) {
    await expect(point).toBeVisible()
  }
})
