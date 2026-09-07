// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'
import { ready, setTheme, v1 } from './helpers'

/**
 * Visual baselines.
 *
 * Two kinds, for two kinds of regression:
 *
 *  - one shot per component block on /op-components, so a change to a
 *    primitive shows up as a diff on that primitive and nothing else. A single
 *    full-page shot of the library would go red on every edit and tell nobody
 *    which component moved.
 *  - full-page shots of the four record pages and the settings hub, which is
 *    where the templates are actually composed and where a token change shows
 *    its real effect.
 *
 * Both in desktop light, desktop dark and phone light: the three combinations
 * the handoff asks people to look at.
 *
 * Regenerate with `bun run e2e:update` — and only from a quiet dev server (no
 * HMR overlay, `bun run lint` clean), because a baseline captured mid-refactor
 * bakes the broken state in.
 */

type Theme = 'light' | 'dark'

/** desktop is reviewed in both themes; the phone baseline is light only. */
function themesFor(projectName: string): Theme[] {
  return projectName === 'phone' ? ['light'] : ['light', 'dark']
}

/**
 * Freeze anything that would make two runs of the same page differ.
 *
 * CSS covers motion and the caret. The DOM part matters just as much: several
 * screens stream their log lines in on an interval (the deploy record, the
 * sandbox, the incident thread), so a screenshot taken at a fixed delay
 * catches a different number of lines each run. Wait for the document to stop
 * mutating instead of waiting for a guessed duration.
 */
async function settle(page: Page) {
  await page.addStyleTag({
    content: `*, *::before, *::after {
      animation: none !important;
      transition: none !important;
      caret-color: transparent !important;
    }`,
  })
  await page.evaluate(async () => {
    await new Promise<void>((resolve) => {
      let timer = 0
      const done = () => { observer.disconnect(); clearTimeout(cap); resolve() }
      const quiet = () => { clearTimeout(timer); timer = window.setTimeout(done, 600) }
      const observer = new MutationObserver(quiet)
      observer.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true })
      // Never hang on a page that ticks forever; 8s is well past every stream
      // in the sandbox.
      const cap = window.setTimeout(done, 8000)
      quiet()
    })
  })
  await page.waitForTimeout(150)
}

/**
 * The deploy record's "Screenshot" panel is an <iframe src="/landing"> scaled
 * down to stand in for a real screenshot service. It is a second copy of the
 * whole app booting inside the page, on its own timeline, so it is masked
 * rather than waited on — there is nothing about the design system to learn
 * from those pixels.
 */
function masks(page: Page) {
  return [page.locator('iframe')]
}

/**
 * The library's blocks, in page order — the same list as the page's own TOC.
 * Spelled out rather than discovered so that a block silently disappearing is
 * a failure, not a quietly smaller run.
 *
 * Top-level blocks only: the Settings demo inside one of them renders its own
 * `<section id>` children, which are not library components.
 */
const BLOCKS: readonly string[] = [
  'status', 'num', 'page-state', 'button-busy', 'copy-action', 'kbd', 'echo', 'chart', 'ledger', 'inspector', 'detail',
  'picker', 'settings', 'mark', 'breakdown', 'callout', 'strip', 'trace', 'logs',
  // The blocks files (src/sections/blocks/), mounted here and in /guide.
  'form-field', 'form-validation', 'form-datetime', 'form-range-schedule', 'form-disabled',
  'notify-table', 'notify-toast', 'notify-attention',
  'viz-choice', 'viz-series', 'viz-legend', 'viz-a11y',
  // DataVizBlocks2, then GenUiBlocks, in the order the page mounts them. Their
  // `DATAVIZ2_TOC` / `GENUI_TOC` exports are the source of truth and the page's
  // rail spreads them; they cannot be imported here, because the module graph
  // behind them reaches a JSON asset that Node will not load without an import
  // attribute. The `toEqual([...BLOCKS])` assertion above is what keeps this
  // copy honest: add a block there and this run goes red until it is added here.
  'viz-band', 'viz-compare', 'viz-stacked', 'viz-heatmap', 'viz-ladder', 'viz-cohort',
  'viz-paths', 'viz-session', 'viz-usage', 'viz-gauge', 'viz-state', 'viz-delta',
  'viz-window', 'viz-topology',
  'genui-ledger', 'genui-provenance', 'genui-proposal', 'genui-streaming',
  'genui-question', 'genui-wrong',
  'content-error', 'content-time', 'content-fmt',
  'tokens-table', 'motion-tiers', 'icons-vocabulary',
]

test.describe('op-components blocks', () => {
  test('the library still has exactly the blocks we shoot', async ({ page }) => {
    await page.goto('/op-components')
    await ready(page)
    const ids = await page
      .locator('main section[id]:not(section section)')
      .evaluateAll((els) => els.map((e) => e.id))
    expect(ids).toEqual([...BLOCKS])
  })

  test('every component block matches its baseline', async ({ page }, testInfo) => {
    test.setTimeout(180_000)
    for (const theme of themesFor(testInfo.project.name)) {
      await setTheme(page, theme)
      await page.goto('/op-components')
      await ready(page)
      await settle(page)

      for (const id of BLOCKS) {
        const block = page.locator(`section#${id}`)
        await block.scrollIntoViewIfNeeded()
        // Soft: one component drifting should not hide the other fifteen.
        await expect.soft(block).toHaveScreenshot(`block-${id}-${theme}.png`, { mask: masks(page) })
      }
    }
  })
})

/**
 * The guide renders four markdown documents through one set of custom
 * renderers, so a change to a renderer (headings, tables, code blocks) shows
 * up everywhere at once. The default section is the baseline: it is the one
 * every reader lands on, and it exercises headings, prose, a table and the
 * live five-rules block in a page short enough to shoot whole.
 */
test('guide matches its baseline', async ({ page }, testInfo) => {
  test.setTimeout(90_000)
  for (const theme of themesFor(testInfo.project.name)) {
    await setTheme(page, theme)
    await page.goto('/guide')
    await ready(page)
    await settle(page)
    await expect(page).toHaveScreenshot(`guide-${theme}.png`, { fullPage: true, mask: masks(page) })
  }
})

const RECORDS: ReadonlyArray<{ label: string; path: string }> = [
  { label: 'deploy-dep_91a', path: v1('deploy:dep_91a') },
  { label: 'db-acme-pg', path: v1('db:acme-pg') },
  { label: 'issue-i_4821', path: v1('issue:i_4821') },
  // The two halves of the monitoring screen: hetzner-2 under pressure with
  // every plot drawn, hetzner-3 silent with every plot greyed and stamped.
  { label: 'node-hetzner-2', path: v1('node:hetzner-2') },
  { label: 'node-hetzner-3', path: v1('node:hetzner-3') },
  { label: 'settings-hub', path: v1('settings') },
  // The logs screen, on the chrome-free console route: query bar, histogram,
  // level toggles and the line list in one shot.
  { label: 'console-logs', path: '/console?p=logs' },
]

for (const { label, path } of RECORDS) {
  test(`${label} matches its baseline`, async ({ page }, testInfo) => {
    test.setTimeout(90_000)
    for (const theme of themesFor(testInfo.project.name)) {
      await setTheme(page, theme)
      await page.goto(path)
      await ready(page)
      await settle(page)
      await expect(page).toHaveScreenshot(`${label}-${theme}.png`, { fullPage: true, mask: masks(page) })
    }
  })
}
