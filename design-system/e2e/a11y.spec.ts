// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import { ready, setTheme, v5 } from './helpers'

/**
 * axe-core over the surfaces the handoff points people at, in both themes.
 *
 * Dark is not a re-skin: it is a second set of token values, so a contrast
 * pair that passes in light can fail in dark and nobody would notice from the
 * light-mode screenshot. Hence every page twice.
 *
 * The gate:
 *   - a serious/critical violation of a rule that is NOT in KNOWN below fails.
 *   - moderate and minor are recorded as test annotations (a backlog, not a
 *     gate — burying them in a red test just gets the file skipped).
 *   - the KNOWN rules are today's standing defects. They are annotated on
 *     every run so they stay visible, and each one names where it comes from.
 *     Fixing one is a `src` change; this suite only refuses to let the list
 *     grow. Delete an entry the moment its cause is fixed.
 */

/**
 * Standing serious/critical findings, 2026-09-06. Rule id → what causes it.
 * Two of these are the sandbox's own chrome, not the design system.
 */
const KNOWN: Record<string, string> = {
  // Empty on purpose: every standing defect found on 2026-09-06 was fixed the same day
  // (Ledger columnheader role, Picker name, PageState status role, HeaderSlotDemo list,
  // Layout archive-link contrast, focusable API <pre> blocks). Add an entry only with the
  // rule id, the cause and the owner, and delete it when the fix lands.
}

const PAGES: ReadonlyArray<{ label: string; path: string }> = [
  { label: '/op-components', path: '/op-components' },
  { label: '/guide', path: '/guide' },
  { label: '/guide#taste', path: '/guide#taste' },
  { label: '/v5?p=projects', path: v5('projects') },
  { label: '/v5?p=api-gateway', path: v5('api-gateway') },
  { label: '/v5?p=deploy:dep_91a', path: v5('deploy:dep_91a') },
  { label: '/v5?p=db:acme-pg', path: v5('db:acme-pg') },
  { label: '/v5?p=errors', path: v5('errors') },
  { label: '/v5?p=issue:i_4821', path: v5('issue:i_4821') },
  { label: '/v5?p=settings', path: v5('settings') },
  { label: '/v5?p=settings:cluster', path: v5('settings:cluster') },
  { label: '/status?project=acme-storefront', path: '/status?project=acme-storefront' },
]

const THEMES = ['light', 'dark'] as const

for (const { label, path } of PAGES) {
  for (const theme of THEMES) {
    test(`${label} has no new serious or critical axe violations (${theme})`, async ({ page }, testInfo) => {
      await setTheme(page, theme)
      await page.goto(path)
      await ready(page)
      // next-themes writes the class on mount; assert it landed rather than
      // silently auditing light twice and calling it dark-mode coverage.
      await expect
        .poll(() => page.evaluate(() => document.documentElement.classList.contains('dark')))
        .toBe(theme === 'dark')
      await page.waitForTimeout(300)

      const results = await new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'])
        .analyze()

      const bad = (i?: string | null) => i === 'serious' || i === 'critical'
      const regressions = results.violations.filter((v) => bad(v.impact) && !(v.id in KNOWN))
      const known = results.violations.filter((v) => bad(v.impact) && v.id in KNOWN)
      const advisory = results.violations.filter((v) => !bad(v.impact))

      for (const v of known) {
        testInfo.annotations.push({
          type: `a11y-known-${v.impact}`,
          description: `${v.id} · ${v.nodes.length} node(s) · ${v.nodes[0]?.target?.join(' ') ?? ''} · ${KNOWN[v.id]}`,
        })
      }
      for (const v of advisory) {
        testInfo.annotations.push({
          type: `a11y-${v.impact ?? 'unknown'}`,
          description: `${v.id} · ${v.nodes.length} node(s) · ${v.nodes[0]?.target?.join(' ') ?? ''} · ${v.help}`,
        })
      }

      const report = regressions
        .map((v) => `${v.impact?.toUpperCase()} ${v.id}: ${v.help}\n` +
          v.nodes.slice(0, 5).map((n) => `    ${n.target.join(' ')}\n      ${n.html.slice(0, 160)}`).join('\n') +
          `\n    ${v.helpUrl}`)
        .join('\n\n')

      expect(
        regressions.map((v) => v.id),
        `${label} (${theme}) has serious/critical axe violations that are not in the known list:\n${report}`,
      ).toEqual([])
    })
  }
}

/**
 * The known list is a debt register, not a wildcard: if a rule stops firing
 * anywhere, its entry must be deleted so the next real occurrence fails.
 */
test('every known-violation entry still has a cause', async ({ page }, testInfo) => {
  test.setTimeout(180_000) // one pass over every page in both themes
  const firing = new Set<string>()
  for (const theme of THEMES) {
    for (const { path } of PAGES) {
      await setTheme(page, theme)
      await page.goto(path)
      await ready(page)
      await page.waitForTimeout(150)
      const results = await new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'])
        .analyze()
      for (const v of results.violations) if (v.id in KNOWN) firing.add(v.id)
    }
  }
  const stale = Object.keys(KNOWN).filter((id) => !firing.has(id))
  for (const id of stale) testInfo.annotations.push({ type: 'a11y-fixed', description: `${id} no longer fires — remove it from KNOWN` })
  expect(stale, `these known violations are fixed; delete them from KNOWN in e2e/a11y.spec.ts: ${stale.join(', ')}`).toEqual([])
})
