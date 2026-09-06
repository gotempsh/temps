// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import { ready, v1 } from './helpers'

/**
 * Transient surfaces: the header's attention panel (Drop + AttentionHost) and
 * the tooltip.
 *
 * Both have the same promise in docs/design-system-handoff.md §6: they open
 * where they are asked to, they close on every ordinary exit (Escape, a click
 * outside, the pointer leaving), and they hand focus back where they took it.
 * A panel that traps focus, or a tooltip the pointer has to keep chasing, is
 * the failure this file is here to catch.
 */

const attentionPanel = 'div[role="dialog"][aria-label="attention"]'

/**
 * Click a point that is genuinely "outside": inside the page body, clear of
 * the open panel, and on nothing interactive. Hard-coded coordinates do not
 * work here — the sandbox's sidebar owns the left edge, and the 480px panel
 * covers the page title at this width — so the point is found by probing
 * `elementFromPoint` down the column until it lands on inert content.
 */
async function clickOutside(page: import('@playwright/test').Page, panelSelector: string) {
  const point = await page.evaluate((sel) => {
    const panel = document.querySelector(sel)
    const main = document.querySelector('main') ?? document.body
    const m = main.getBoundingClientRect()
    const x = Math.round(m.left + m.width / 2)
    for (let y = Math.round(m.top + 16); y < Math.min(window.innerHeight - 8, m.bottom); y += 12) {
      const el = document.elementFromPoint(x, y)
      if (!el) continue
      if (panel?.contains(el)) continue
      if (el.closest('a, button, input, select, textarea, label, [role="button"], [role="tab"], [tabindex]')) continue
      return { x, y }
    }
    return null
  }, panelSelector)
  if (!point) throw new Error('no inert point found to click outside the panel')
  await page.mouse.click(point.x, point.y)
}

test.describe('attention panel', () => {
  test('opens from the header badge and closes on Escape, returning focus', async ({ page }) => {
    await page.goto(v1('errors'))
    await ready(page)

    const button = page.locator('button[aria-haspopup="dialog"][aria-expanded]').first()
    await expect(button).toBeVisible()
    await expect(button).toHaveAttribute('aria-expanded', 'false')

    const panel = page.locator(attentionPanel)
    await expect(panel).toBeHidden()

    await button.click()
    await expect(button).toHaveAttribute('aria-expanded', 'true')
    await expect(panel).toBeVisible()

    // Focus moves into the panel on open, so the reader is already inside it.
    const inside = await page.evaluate((sel) => {
      const p = document.querySelector(sel)
      return !!p && !!document.activeElement && p.contains(document.activeElement)
    }, attentionPanel)
    expect(inside, 'focus moves into the panel on open').toBe(true)

    await page.keyboard.press('Escape')
    await expect(panel).toBeHidden()
    await expect(button).toHaveAttribute('aria-expanded', 'false')
    await expect(button).toBeFocused()
  })

  test('closes on a click outside', async ({ page }) => {
    await page.goto(v1('errors'))
    await ready(page)

    const button = page.locator('button[aria-haspopup="dialog"][aria-expanded]').first()
    const panel = page.locator(attentionPanel)

    await button.click()
    await expect(panel).toBeVisible()

    await clickOutside(page, attentionPanel)
    await expect(panel).toBeHidden()
    await expect(button).toHaveAttribute('aria-expanded', 'false')
  })

  test('a click inside the panel leaves it open', async ({ page }) => {
    await page.goto(v1('errors'))
    await ready(page)

    const button = page.locator('button[aria-haspopup="dialog"][aria-expanded]').first()
    const panel = page.locator(attentionPanel)

    await button.click()
    await expect(panel).toBeVisible()

    const box = await panel.boundingBox()
    expect(box).not.toBeNull()
    await page.mouse.click(box!.x + box!.width - 8, box!.y + 4)
    await expect(panel).toBeVisible()
  })
})

test.describe('tooltip', () => {
  /** The linked-project marks on a database record: a row of favicons, name on hover. */
  async function firstMark(page: import('@playwright/test').Page) {
    const dd = page.locator('dt', { hasText: /^linked projects$/ }).locator('xpath=following-sibling::dd[1]')
    await expect(dd).toBeVisible()
    return dd.locator('button[aria-label]').first()
  }

  test('opens on hover with no animation and closes when the pointer leaves', async ({ page }) => {
    await page.goto(v1('db:acme-pg'))
    await ready(page)

    const mark = await firstMark(page)
    const name = await mark.getAttribute('aria-label')
    expect(name).toBeTruthy()

    await mark.hover()
    const tip = page.getByRole('tooltip', { name: name! })
    await expect(tip).toBeVisible()

    // "A tooltip is a label, not a surface: it appears, it does not fly in."
    const animation = await tip.evaluate((el) => {
      const s = getComputedStyle(el)
      return { name: s.animationName, duration: s.animationDuration, transition: s.transitionProperty }
    })
    expect(animation.name).toBe('none')

    // Hoverable content is off, so drifting the pointer down through the label
    // and away must close it rather than keep it pinned.
    const box = await tip.boundingBox()
    expect(box).not.toBeNull()
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2)
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height + 240)
    await expect(tip).toBeHidden()
  })

  test('closes when the pointer moves straight off the trigger', async ({ page }) => {
    await page.goto(v1('db:acme-pg'))
    await ready(page)

    const mark = await firstMark(page)
    const name = await mark.getAttribute('aria-label')

    await mark.hover()
    const tip = page.getByRole('tooltip', { name: name! })
    await expect(tip).toBeVisible()

    await page.mouse.move(4, 4)
    await expect(tip).toBeHidden()
  })
})
