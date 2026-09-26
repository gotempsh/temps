// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from '@playwright/test'

test('console page headers share gutters, typography, and top spacing', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1000 })
  let reference: { x: number; y: number } | undefined
  for (const route of [
    '/proxy-logs',
    '/logs',
    '/traces',
    '/errors',
    '/analytics',
    '/audit-logs',
    '/domains',
    '/settings/users',
  ]) {
    await page.goto(route)
    const header = page.locator('[data-page-header]').first()
    await expect(header).toBeVisible()
    const heading = header.locator('h1')
    await expect(heading).toHaveCSS('font-size', '24px')
    const bounds = await header.evaluate((element) => {
      const container = element.closest('[data-page-container]')!
      const outer = container.getBoundingClientRect()
      const inner = element.getBoundingClientRect()
      return { x: inner.x - outer.x, y: inner.y - outer.y }
    })
    if (reference) {
      expect(bounds!.x).toBe(reference.x)
      expect(bounds!.y).toBe(reference.y)
    } else reference = bounds!
  }
  await page.goto('/proxy-logs')
  await expect(page.locator('[data-page-header]')).toBeVisible()
  await page.screenshot({
    path: '/tmp/temps-standard-page-header-desktop.png',
    fullPage: true,
  })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.locator('[data-page-header] h1')).toBeVisible()
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth
    )
  ).toBe(true)
  await page.screenshot({
    path: '/tmp/temps-standard-page-header-mobile.png',
    fullPage: true,
  })
})
