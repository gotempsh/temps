// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

for (const width of [320, 390, 1440]) {
  test(`project analytics keeps metrics, long paths and tabs readable at ${width}px`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    test.skip(!projects[0], 'Requires a test project')
    const project = projects[0]
    const longPath =
      '/products/a-long-readable-page-name/with-many-segments-and-query-parameters'
    await page.setViewportSize({ width, height: 1000 })
    await page.route(/\/has-events(?:\?|$)/, (route) =>
      route.fulfill({ json: { has_events: true } })
    )
    await page.route(/\/unique-counts(?:\?|$)/, (route) =>
      route.fulfill({ json: { count: 1234567 } })
    )
    await page.route(/\/events\/properties\/breakdown(?:\?|$)/, (route) =>
      route.fulfill({
        json: { items: [{ value: longPath, count: 24 }], total: 24 },
      })
    )
    await page.goto(`/projects/${project.slug}/analytics?filter=7days`)
    const summary = page.getByLabel('Analytics summary')
    await expect(summary).toHaveAttribute('aria-busy', 'false')
    await expect(summary.locator('[title="1,234,567"]').first()).toBeVisible()
    const summaryBox = await summary.boundingBox()
    expect(summaryBox!.height).toBeLessThan(145)
    expect(summaryBox!.x + summaryBox!.width).toBeLessThanOrEqual(width)
    const list = page.getByRole('list', { name: 'Top pages by visitors' })
    await expect(list.getByRole('link', { name: longPath })).toBeVisible()
    await expect(list.getByRole('link', { name: longPath })).toHaveAttribute(
      'href',
      /filter=7days/
    )
    const box = await list.boundingBox()
    expect(box!.x + box!.width).toBeLessThanOrEqual(width)
    const tabs = page.getByRole('tablist', { name: 'Analytics breakdowns' })
    await tabs.getByRole('tab', { name: 'Traffic', exact: true }).focus()
    await page.keyboard.press('ArrowRight')
    await expect(
      tabs.getByRole('tab', { name: 'Audience', exact: true })
    ).toHaveAttribute('aria-selected', 'true')
    await tabs.getByRole('tab', { name: 'Events', exact: true }).click()
    await expect(
      tabs.getByRole('tab', { name: 'Events', exact: true })
    ).toHaveAttribute('aria-selected', 'true')
    await tabs.getByRole('tab', { name: 'Traffic', exact: true }).click()
    await expect(list).toBeVisible()
    await page.screenshot({
      path: `/tmp/temps-project-analytics-presentation-${width}.png`,
      fullPage: true,
    })
  })
}

test('analytics metric errors offer retry and recover', async ({ page }) => {
  const { projects } = await (await page.request.get('/api/projects')).json()
  test.skip(!projects[0], 'Requires a test project')
  let fail = true
  await page.route(/\/has-events(?:\?|$)/, (route) =>
    route.fulfill({ json: { has_events: true } })
  )
  await page.route(/\/unique-counts(?:\?|$)/, (route) =>
    fail
      ? route.fulfill({ status: 403, json: { detail: 'Metrics unavailable' } })
      : route.fulfill({ json: { count: 42 } })
  )
  await page.goto(`/projects/${projects[0].slug}/analytics`)
  await expect(
    page.getByText('Could not load analytics metrics.')
  ).toBeVisible()
  fail = false
  await page.getByRole('button', { name: 'Retry metrics' }).click()
  await expect(page.getByLabel('Analytics summary')).toContainText('42')
})
