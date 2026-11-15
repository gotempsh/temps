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

/** The environment picker in the project bar above every project page. */
function environmentPicker(page: import('@playwright/test').Page) {
  return page
    .getByRole('navigation', { name: 'Project and environment' })
    .getByRole('combobox', { name: 'Environment', exact: true })
}

test('switching environments in the project bar keeps the selected environment page', async ({
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
  await page.goto(
    `/projects/${project.slug}/environments?env=${production.slug}&view=metrics`
  )
  const tabs = (name: string) =>
    page.getByRole('tablist', { name: `${name} pages` })
  await expect(tabs(production.name).getByRole('tab')).toHaveText([
    'Containers',
    'Metrics',
    'Shield',
    'Settings',
  ])
  await expect(
    tabs(production.name).getByRole('tab', { name: 'Metrics', exact: true })
  ).toHaveAttribute('aria-selected', 'true')
  // Real metrics must render SVG plots, not just correctly sized empty wrappers.
  const charts = page.locator('[data-chart] .recharts-surface')
  await expect(charts).toHaveCount(3)
  await expect(page.locator('.recharts-line-curve').first()).toBeVisible()
  for (const chart of await charts.all()) {
    const bounds = await chart.boundingBox()
    expect(bounds?.width).toBeGreaterThan(100)
    expect(bounds?.height).toBeGreaterThan(100)
  }
  await expect(environmentPicker(page)).toContainText(production.name)

  await environmentPicker(page).click()
  await page.getByRole('option', { name: /^staging/ }).click()
  await expect(page).toHaveURL(/[?&]env=staging(&|$)/)
  await expect(page).toHaveURL(/[?&]view=metrics(&|$)/)
  await expect(
    tabs('staging').getByRole('tab', { name: 'Metrics', exact: true })
  ).toHaveAttribute('aria-selected', 'true')

  await tabs('staging')
    .getByRole('tab', { name: 'Settings', exact: true })
    .click()
  await expect(page).toHaveURL(/[?&]view=settings(&|$)/)
  await page.reload()
  await expect(
    tabs('staging').getByRole('tab', { name: 'Settings', exact: true })
  ).toHaveAttribute('aria-selected', 'true')
  await environmentPicker(page).click()
  await page
    .getByRole('option', { name: new RegExp(`^${production.name}`) })
    .click()
  await expect(page).toHaveURL(new RegExp(`[?&]env=${production.slug}(&|$)`))
  await expect(page).toHaveURL(/[?&]view=settings(&|$)/)
})

test('a link that names the environment by id still opens it', async ({
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
    id: 999992,
    name: 'staging',
    slug: 'staging',
  }
  await page.route(`**/api/projects/${project.id}/environments`, (route) =>
    route.fulfill({ json: [production, staging] })
  )
  await page.route(
    `**/api/projects/${project.id}/environments/${staging.id}`,
    (route) => route.fulfill({ json: staging })
  )
  // Alarms, Shield and reachability link with `?environment=<id>`.
  await page.goto(
    `/projects/${project.slug}/environments?environment=${staging.id}&view=settings`
  )
  await expect(environmentPicker(page)).toContainText('staging')
  await expect(
    page
      .getByRole('tablist', { name: 'staging pages' })
      .getByRole('tab', { name: 'Settings', exact: true })
  ).toHaveAttribute('aria-selected', 'true')
})

test('on a phone the environment tabs and "New environment" fit the screen', async ({
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
  await page.getByRole('button', { name: 'New environment' }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.press('Escape')
  await page
    .getByRole('tablist', { name: `${environment.name} pages` })
    .getByRole('tab', { name: 'Metrics', exact: true })
    .click()
  await expect(page).toHaveURL(/view=metrics/)
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  ).toBe(true)
  await page.screenshot({
    path: '/tmp/temps-environment-tabs-mobile.png',
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
