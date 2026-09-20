// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

test.beforeEach(async ({ page }) => {
  // All API requests are intercepted: this suite never reads or changes live data.
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    let json: unknown = []
    if (path === '/api/user/me') {
      json = {
        id: 42,
        name: 'Test Operator',
        email: 'operator@example.com',
        role: 'admin',
      }
    } else if (path === '/api/logs/search') {
      json = {
        lines: [
          {
            chunk_id: 'sample',
            line_offset: 0,
            timestamp: '2026-09-20T12:00:00Z',
            level: 'INFO',
            owner: 'sample',
            service: 'web',
            env: 'production',
            message: 'Sample log message',
          },
        ],
        scan_limit_reached: false,
      }
    }
    await route.fulfill({ json })
  })
})

async function expectFacets(page: Page, visible: boolean) {
  await expect(
    page.getByRole('button', { name: 'Filters', exact: true })
  ).toHaveAttribute('aria-expanded', String(visible))
  const panel = page.getByRole('complementary', { name: 'Log facets' })
  if (visible) {
    await expect(panel).toBeVisible()
    await expect(
      panel.getByText('Counts from this page only.', { exact: false })
    ).toBeVisible()
  } else {
    await expect(panel).toHaveCount(0)
  }
}

test('mobile starts collapsed and the toggle persists through reload and resize', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/logs')
  await expectFacets(page, false)
  const toggle = page.getByRole('button', { name: 'Filters', exact: true })
  await toggle.click()
  await expectFacets(page, true)
  await expect(page).toHaveURL(/[?&]facets=1(?:&|$)/)
  await page.reload()
  await expectFacets(page, true)
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expectFacets(page, true)
  await toggle.click()
  await expectFacets(page, false)
  await expect(page).toHaveURL(/[?&]facets=0(?:&|$)/)
  await page.setViewportSize({ width: 390, height: 844 })
  await expectFacets(page, false)
})

test('default visibility follows both directions across the 1280px breakpoint without navigation', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1279, height: 1000 })
  await page.goto('/logs')
  await expectFacets(page, false)
  const initialUrl = page.url()
  for (const width of [1280, 1279, 1440, 390]) {
    await page.setViewportSize({ width, height: 1000 })
    await expectFacets(page, width >= 1280)
    expect(page.url()).toBe(initialUrl)
  }
})

for (const preference of ['0', '1']) {
  test(`explicit facets=${preference} overrides viewport defaults and transitions`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(`/logs?facets=${preference}`)
    await expectFacets(page, preference === '1')
    for (const width of [1280, 1279, 1440, 390]) {
      await page.setViewportSize({ width, height: 1000 })
      await expectFacets(page, preference === '1')
      expect(new URL(page.url()).searchParams.get('facets')).toBe(preference)
    }
  })
}
