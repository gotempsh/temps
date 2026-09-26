// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect } from '../fixtures'
import { settingsNavigationGroups } from '../../src/components/settings/settings-navigation'

const destinations = [
  ...settingsNavigationGroups.flatMap((group) =>
    group.items.map((item) => item.url)
  ),
  '/git-providers',
  '/tools',
  '/settings/users/new',
  '/settings/keys/new',
  '/settings/notifications/new',
  '/settings/notifications/routes/new',
  '/settings/load-balancer/add',
  '/settings/auth/new',
]

for (const width of [390, 768, 1920]) {
  test(`sidebar pages share full-width content and one set of gutters at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1080 })

    for (const path of destinations) {
      await test.step(path, async () => {
        await page.goto(path)
        const shell = page.locator('[data-page-container]')
        await expect(shell).toHaveCount(1)
        const content = shell.locator(':scope > div').first()
        await expect(content.locator(':scope > *').first()).toBeVisible()
        const dimensions = await shell.evaluate((element) => {
          const inner = element.firstElementChild as HTMLElement
          const child = inner.firstElementChild as HTMLElement
          const outerStyle = getComputedStyle(element)
          const innerStyle = getComputedStyle(inner)
          const childStyle = getComputedStyle(child)
          return {
            gutter: Number.parseFloat(outerStyle.paddingLeft),
            maxWidth: innerStyle.maxWidth,
            innerWidth: inner.getBoundingClientRect().width,
            childWidth: child.getBoundingClientRect().width,
            childPadding: Number.parseFloat(childStyle.paddingLeft),
            childIsCard: child.getAttribute('data-slot') === 'card',
            overflow: document.documentElement.scrollWidth > window.innerWidth,
          }
        })
        expect(dimensions.gutter).toBe(
          width >= 1024 ? 32 : width >= 640 ? 24 : 16
        )
        expect(dimensions.maxWidth).toBe('none')
        expect(
          Math.abs(dimensions.innerWidth - dimensions.childWidth)
        ).toBeLessThan(2)
        // Cards may pad their contents, but page wrappers must not add gutters.
        if (!dimensions.childIsCard) expect(dimensions.childPadding).toBe(0)
        expect(dimensions.overflow).toBe(false)
      })
    }
  })
}
