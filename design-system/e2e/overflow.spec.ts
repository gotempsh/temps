// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'
import { STANDALONE_ROUTES, V5_VIEWS_WITH_FLAGS, formatNotes, ready, v5, watchConsole } from './helpers'

/**
 * Two things every route owes the reader, at every width the system claims to
 * support: the page does not scroll sideways, and it boots without complaining.
 *
 * Horizontal overflow is the failure mode of a grid system with fixed tracks —
 * one unbreakable identifier, one table that forgot `min-w-0`, and the whole
 * document slides. It is invisible on a 1440 desktop and unusable on a phone,
 * which is why the sweep runs at both widths.
 *
 * The console's own scrollers (`op-scroll-x` tab strips, action bars, wide
 * ledgers) are allowed to scroll; the *document* is not.
 */

type Route = { path: string; label: string }

const ROUTES: Route[] = [
  ...V5_VIEWS_WITH_FLAGS.map(({ view, extra, label }) => ({ path: v5(view, extra), label: `/v5?p=${label}` })),
  ...STANDALONE_ROUTES,
]

for (const route of ROUTES) {
  test(`${route.label} lays out without horizontal overflow and boots clean`, async ({ page }, testInfo) => {
    const notes = watchConsole(page)

    await page.goto(route.path)
    await ready(page)
    // Charts, maps and the ledger's own measuring pass all settle a frame or
    // two after mount; measure once the layout has stopped moving.
    await page.waitForTimeout(400)

    const metrics = await page.evaluate(() => {
      const doc = document.documentElement
      // The widest element that sticks out, so a failure names a culprit
      // instead of just a number.
      let worst: { selector: string; right: number } | null = null
      const limit = doc.clientWidth
      for (const el of Array.from(document.body.querySelectorAll<HTMLElement>('*'))) {
        const r = el.getBoundingClientRect()
        if (r.width === 0 || r.height === 0) continue
        if (r.right <= limit + 1) continue
        // Ignore anything inside a deliberate horizontal scroller.
        if (el.closest('.op-scroll-x, [data-allow-overflow]')) continue
        if (!worst || r.right > worst.right) {
          const id = el.id ? `#${el.id}` : ''
          const cls = typeof el.className === 'string' ? `.${el.className.trim().split(/\s+/).slice(0, 3).join('.')}` : ''
          worst = { selector: `${el.tagName.toLowerCase()}${id}${cls}`, right: Math.round(r.right) }
        }
      }
      return { scrollWidth: doc.scrollWidth, clientWidth: doc.clientWidth, worst }
    })

    expect(
      metrics.scrollWidth,
      `${route.label} at ${testInfo.project.use.viewport?.width}px scrolls sideways` +
        (metrics.worst ? ` — widest offender ${metrics.worst.selector} reaches ${metrics.worst.right}px` : ''),
    ).toBeLessThanOrEqual(metrics.clientWidth)

    expect(notes, `${route.label} wrote to the console:\n${formatNotes(notes)}`).toEqual([])
  })
}
