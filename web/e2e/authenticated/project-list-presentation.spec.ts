// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'
import {
  PROJECT_GROUPS_LIST_API,
  PROJECTS_LIST_API,
  projectGroupsListBody,
  serviceFixture,
  servicesListBody,
} from '../fixtures'

const services = ['Billing API', 'Documentation', 'Storefront'].map(
  (name, index) => serviceFixture(900001 + index, name)
)

/** No project groups: every service is listed on its own. */
async function mockNoGroups(page: Page) {
  await page.route(PROJECT_GROUPS_LIST_API, (route) =>
    route.fulfill({ json: projectGroupsListBody([]) })
  )
}

for (const width of [1440, 390]) {
  test(`service search survives reload and clears at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1000 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    const searches: (string | null)[] = []
    await mockNoGroups(page)
    await page.route(PROJECTS_LIST_API, (route) => {
      const url = new URL(route.request().url())
      searches.push(url.searchParams.get('search'))
      return route.fulfill({ json: servicesListBody(url, services) })
    })
    await page.goto('/projects')
    const search = page.getByRole('textbox', {
      name: 'Filter projects and services by name or slug',
    })
    await expect(search).toBeVisible()
    await search.fill('billing')
    await expect(page).toHaveURL(/q=billing/)
    // The filter is applied by the server, not by hiding cards client-side.
    await expect.poll(() => searches).toContain('billing')
    await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
    await expect(page.getByRole('link', { name: /Documentation/ })).toHaveCount(
      0
    )
    await page.reload()
    await expect(search).toHaveValue('billing')
    await search.fill('missing-service')
    await expect(
      page.getByText('No matching projects or services', { exact: true })
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

test('service load failure offers retry instead of claiming no matching services', async ({
  page,
}) => {
  let fail = true
  await mockNoGroups(page)
  await page.route(PROJECTS_LIST_API, (route) =>
    fail
      ? route.fulfill({ status: 503, json: { title: 'Services unavailable' } })
      : route.fulfill({
          json: servicesListBody(new URL(route.request().url()), services),
        })
  )
  await page.goto('/projects')
  await expect(
    page.getByText('Services could not be loaded', { exact: true })
  ).toBeVisible({ timeout: 30000 })
  await expect(
    page.getByText('No matching projects or services', { exact: true })
  ).toHaveCount(0)
  // Groups loaded fine, so their own failure notice must not show.
  await expect(page.getByText(/^Projects could not be loaded/)).toHaveCount(0)
  fail = false
  await page.getByRole('button', { name: 'Retry loading services' }).click()
  await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
  await expect(
    page.getByText('Services could not be loaded', { exact: true })
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
  await mockNoGroups(page)
  await page.route(PROJECTS_LIST_API, async (route) => {
    await held
    await route.fulfill({
      json: servicesListBody(new URL(route.request().url()), services),
    })
  })
  try {
    await page.goto('/projects')
    const loading = page.locator('[aria-label="Loading projects and services"]')
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
      page.getByRole('textbox', {
        name: 'Filter projects and services by name or slug',
      })
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
