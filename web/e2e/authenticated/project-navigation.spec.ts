// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from '@playwright/test'

test('direct project links and direct flat settings navigation', async ({
  page,
}) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  expect(projects.length).toBeGreaterThan(0)
  const root = `/projects/${projects[0].slug}`
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(`${root}/settings`)
  await expect(page).toHaveURL(/settings\/general$/)
  const primary = page.getByRole('list', { name: 'Project navigation' })
  await expect(primary.getByRole('link')).toHaveCount(11)
  await expect(primary.getByRole('link')).toHaveText([
    'Overview',
    'Deployments',
    'Environments',
    'Logs',
    'Errors',
    'Traces',
    'Analytics',
    'Monitoring',
    'Databases',
    'Security',
    'Settings',
  ])
  const settings = page.getByRole('navigation', { name: 'Settings pages' })
  await expect(settings.getByRole('link')).toHaveCount(7)
  await expect(settings.locator('a > svg')).toHaveCount(7)
  await settings
    .getByRole('link', { name: 'Build & deploy', exact: true })
    .click()
  await expect(page).toHaveURL(/settings\/delivery$/)
  await expect(
    page.getByRole('heading', { name: 'Build & deploy', exact: true })
  ).toBeVisible()
  await page
    .locator('summary')
    .filter({ hasText: /^Build$/ })
    .click()
  await expect(
    page.getByRole('heading', { name: 'Build', exact: true })
  ).toBeVisible()
  await expect(
    settings.getByRole('link', { name: 'Build & deploy', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await expect(
    page.getByRole('tab', { name: 'Previews', exact: true })
  ).toHaveCount(0)
  await page
    .locator('summary')
    .filter({ hasText: /^Deployment$/ })
    .click()
  await expect(
    page.getByRole('heading', { name: 'Deployment', exact: true })
  ).toBeVisible()
  await expect(page).toHaveURL(/settings\/delivery$/)
  await page.screenshot({
    path: '/tmp/temps-project-settings-compact-icons.png',
    fullPage: true,
  })
  await settings.getByRole('link', { name: 'Variables', exact: true }).click()
  await expect(
    page.locator('summary').filter({ hasText: /^Secrets$/ })
  ).toBeVisible()
  await page.goBack()
  await expect(
    settings.getByRole('link', { name: 'Build & deploy', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await primary.getByRole('link', { name: 'Security', exact: true }).click()
  await expect(
    primary.getByRole('link', { name: 'Security', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  const security = page.getByRole('navigation', { name: 'Security pages' })
  await security.getByRole('link', { name: 'Protection', exact: true }).click()
  await expect(page).toHaveURL(/settings\/security$/)
  await expect(
    primary.getByRole('link', { name: 'Security', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await security.getByRole('link', { name: 'Access', exact: true }).click()
  await expect(page).toHaveURL(/settings\/access$/)
  await page.screenshot({
    path: '/tmp/temps-project-security-flat.png',
    fullPage: true,
  })
  for (const [section, count] of [
    ['Logs', 3],
    ['Traces', 2],
    ['Monitoring', 3],
    ['Analytics', 10],
  ] as const) {
    await primary.getByRole('link', { name: section, exact: true }).click()
    const nav = page.getByRole('navigation', { name: `${section} pages` })
    await expect(nav.getByRole('link')).toHaveCount(count)
    await expect(nav.locator('a > svg')).toHaveCount(count)
    await expect(
      primary.getByRole('link', { name: section, exact: true })
    ).toHaveAttribute('aria-current', 'page')
  }
  await page.screenshot({
    path: '/tmp/temps-project-analytics-navigation.png',
    fullPage: true,
  })
  await primary.getByRole('link', { name: 'Monitoring', exact: true }).click()
  await page
    .getByRole('navigation', { name: 'Monitoring pages' })
    .getByRole('link', { name: 'Alert rules' })
    .click()
  await expect(
    primary.getByRole('link', { name: 'Monitoring', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await page.screenshot({
    path: '/tmp/temps-project-monitoring-navigation.png',
    fullPage: true,
  })
  expect(errors).toEqual([])
})

test('legacy build links and mobile contextual navigation stay usable', async ({
  page,
}) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  expect(projects.length).toBeGreaterThan(0)
  const root = `/projects/${projects[0].slug}`
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(`${root}/settings/build?tab=previews`)
  await expect(
    page.getByRole('heading', { name: 'Preview environments', exact: true })
  ).toBeVisible()
  await page
    .getByRole('button', { name: 'Settings pages', exact: true })
    .click()
  const dialog = page.getByRole('dialog')
  await expect(
    dialog.getByRole('link', { name: 'Build & deploy', exact: true })
  ).toHaveAttribute('aria-current', 'page')
  await dialog.getByRole('link', { name: 'Automation', exact: true }).click()
  await expect(page).toHaveURL(/settings\/automation$/)
  await expect(dialog).not.toBeVisible()
  await page
    .locator('summary')
    .filter({ hasText: /^Cron jobs$/ })
    .click()
  await expect(page).toHaveURL(/settings\/automation$/)
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  ).toBe(true)
})
