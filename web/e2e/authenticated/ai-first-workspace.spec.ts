// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, expectAppMounted, test } from '../fixtures'

test.describe('AI workspace', () => {
  for (const width of [768, 1024, 1279]) {
    test(`keeps workspace controls usable at ${width}px`, async ({
      page,
      consoleErrors,
    }) => {
      await page.setViewportSize({ width, height: 900 })
      await page.goto('/ai-first')
      await expectAppMounted(page)

      const workspaceBadge = page
        .locator('header')
        .getByText('AI workspace', { exact: true })
      if (width >= 1024) {
        await expect(workspaceBadge).toBeVisible()
      } else {
        await expect(workspaceBadge).toBeHidden()
      }
      await expect(
        page.getByRole('button', { name: 'New workspace' })
      ).toBeVisible()
      await expect(
        page.getByRole('link', { name: /Harnesses/ })
      ).toHaveAttribute('href', '/agent-sandbox/providers')

      await page.getByRole('button', { name: 'New workspace' }).click()
      await expect(
        page.getByRole('heading', { name: 'Start a persistent machine.' })
      ).toBeVisible()

      const viewportOverflow = await page.evaluate(
        () => document.documentElement.scrollWidth - window.innerWidth
      )
      expect(
        viewportOverflow,
        `the AI workspace must not overflow horizontally at ${width}px`
      ).toBeLessThanOrEqual(0)
      expect(consoleErrors).toEqual([])
    })
  }
})
