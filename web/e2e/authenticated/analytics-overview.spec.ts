// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from '@playwright/test'

for (const theme of ['light', 'dark']) {
  test(`global analytics renders sparse traffic and shared controls in ${theme}`, async ({
    page,
  }) => {
    await page.addInitScript(
      (theme) => localStorage.setItem('theme', theme),
      theme
    )
    await page.route('**/api/analytics/global?**', async (route) => {
      const facet = new URL(route.request().url()).searchParams.get('facet')
      await route.fulfill({
        json: {
          rows: [
            {
              key: facet === 'traffic' ? '2026-09-10T11:00:00Z' : '/',
              project_id: 4,
              project_name: 'Example',
              views: 3,
              visitors: 2,
              sessions: 1,
              avg_time_seconds: 0,
              bounce_rate: 0,
            },
          ],
          total: 1,
          total_views: 3,
        },
      })
    })
    await page.goto(
      '/analytics?range=custom&from=2026-09-10T09:00:00Z&to=2026-09-10T13:00:00Z'
    )
    await expect(
      page.getByRole('group', { name: 'Traffic metric' })
    ).toBeVisible()
    const curve = page.locator('.recharts-line-curve').first()
    await expect(curve).toBeVisible()
    const original = await curve.getAttribute('d')
    expect(original?.match(/[LC]/g)?.length).toBeGreaterThanOrEqual(4)
    await page
      .getByRole('group', { name: 'Traffic metric' })
      .getByRole('button', { name: 'Page views' })
      .click()
    await expect(
      page.getByRole('heading', { name: 'Hourly Page Views' })
    ).toBeVisible()
    for (const tab of ['Traffic', 'Audience', 'Technology', 'Events']) {
      await page.getByRole('tab', { name: tab, exact: true }).click()
      await expect(
        page.getByRole('tab', { name: tab, exact: true })
      ).toHaveAttribute('data-state', 'active')
    }
    await page.getByRole('tab', { name: 'Traffic', exact: true }).click()
    await page.locator('[data-page-header]').scrollIntoViewIfNeeded()
    await expect(page.locator('html')).toHaveClass(new RegExp(theme))
    await page.screenshot({
      path: `/tmp/temps-global-analytics-${theme}.png`,
      fullPage: true,
    })
  })
}
