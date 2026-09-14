// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

const projects = ['Billing API', 'Documentation', 'Storefront'].map(
  (name, index) => ({
    id: 900001 + index,
    name,
    slug: name.toLowerCase().replace(/ /g, '-'),
    source_type: 'git',
    project_type: 'app',
    main_branch: 'main',
    directory: '.',
    preset: 'nextjs',
    created_at: 1788951600,
    updated_at: 1788951600,
    deployment_config: {},
    last_deployment: null,
  })
)

for (const width of [1440, 390]) {
  test(`project search survives reload and clears at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1000 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await page.route(/\/api\/projects(?:\?|$)/, (route) =>
      route.fulfill({ json: { projects, total: 3 } })
    )
    await page.goto('/projects')
    const search = page.getByRole('textbox', {
      name: 'Filter projects by name or slug',
    })
    await expect(search).toBeVisible()
    await search.fill('billing')
    await expect(page).toHaveURL(/q=billing/)
    await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
    await expect(page.getByRole('link', { name: /Documentation/ })).toHaveCount(
      0
    )
    await page.reload()
    await expect(search).toHaveValue('billing')
    await search.fill('missing-project')
    await expect(
      page.getByText('No matching projects', { exact: true })
    ).toBeVisible()
    await page
      .getByRole('button', { name: 'Clear filter', exact: true })
      .click()
    await expect(search).toHaveValue('')
    await expect(
      page.getByRole('link', { name: /Documentation/ })
    ).toBeVisible()
    await expect(page).not.toHaveURL(/[?&]q=/)
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth
      )
    ).toBe(true)
    expect(errors).toEqual([])
    await page.screenshot({
      path: `/tmp/temps-projects-${width}.png`,
      fullPage: true,
    })
  })
}

test('project load failure offers retry instead of claiming no matching projects', async ({
  page,
}) => {
  let fail = true
  await page.route(/\/api\/projects(?:\?|$)/, (route) =>
    fail
      ? route.fulfill({ status: 503, json: { title: 'Projects unavailable' } })
      : route.fulfill({ json: { projects, total: 3 } })
  )
  await page.goto('/projects')
  await expect(
    page.getByText('Projects could not be loaded', { exact: true })
  ).toBeVisible({ timeout: 30000 })
  await expect(
    page.getByText('No matching projects', { exact: true })
  ).toHaveCount(0)
  fail = false
  await page.getByRole('button', { name: 'Retry loading projects' }).click()
  await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
  await expect(
    page.getByText('Projects could not be loaded', { exact: true })
  ).toHaveCount(0)
})

test('loading keeps a responsive card grid before results arrive', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 })
  let release: () => void = () => {}
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(/\/api\/projects(?:\?|$)/, async (route) => {
    await held
    await route.fulfill({ json: { projects, total: 3 } })
  })
  try {
    await page.goto('/projects')
    const loading = page.locator('[aria-label="Loading projects"]')
    await expect(loading).toBeVisible()
    await expect(loading.locator(':scope > div')).toHaveCount(9)
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth
      )
    ).toBe(true)
    release()
    await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
    await expect(loading).toHaveCount(0)
    await page.evaluate(() => {
      localStorage.setItem('theme', 'dark')
    })
    await expect(
      page.getByRole('textbox', { name: 'Filter projects by name or slug' })
    ).toBeVisible()
    await page.reload()
    await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
    await expect(page.locator('html')).toHaveClass(/dark/)
    await page.screenshot({
      animations: 'disabled',
      path: '/tmp/temps-projects-dark-390.png',
      fullPage: true,
    })
  } finally {
    release()
  }
})
