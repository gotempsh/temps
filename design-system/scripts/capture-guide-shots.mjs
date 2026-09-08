// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Capture the pair of screenshots the guide's "Night is not paper inverted"
 * subsection is about: one real console screen, the same screen, two token
 * sets. They are checked in under `public/guide/` because the guide has to
 * show the two next to each other and no CSS trick puts two themes on one
 * page at full size.
 *
 * They are the one place in the guide that is a picture rather than a live
 * render, so they go stale — regenerate them whenever the dark tokens move:
 *
 *   bun run dev                       # or any server on DS_PORT
 *   node scripts/capture-guide-shots.mjs
 *
 * DS_PORT defaults to 5186, matching `bun run e2e`.
 */
import { mkdir } from 'node:fs/promises'
import { chromium } from '@playwright/test'

const PORT = process.env.DS_PORT ?? '5186'
const BASE = `http://localhost:${PORT}`
const OUT = new URL('../public/guide/', import.meta.url)

// One screen, chosen because it carries every part the four corrections touch:
// framed sections, a raised popover-free surface, a sampled band, and state hues.
const SHOTS = [
  { theme: 'light', file: 'night-light.png' },
  { theme: 'dark', file: 'night-dark.png' },
]
const PATH = '/console?p=api-gateway'

await mkdir(OUT, { recursive: true })
const browser = await chromium.launch()
try {
  for (const shot of SHOTS) {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      deviceScaleFactor: 1,
      colorScheme: shot.theme,
    })
    const page = await context.newPage()
    // next-themes reads storage before paint; setting it avoids a flash of the
    // wrong theme baked into the image.
    await page.addInitScript((t) => window.localStorage.setItem('theme', t), shot.theme)
    await page.goto(BASE + PATH, { waitUntil: 'networkidle' })
    await page.waitForTimeout(600)
    const isDark = await page.evaluate(() => document.documentElement.classList.contains('dark'))
    if (isDark !== (shot.theme === 'dark')) throw new Error(`${shot.file}: page is ${isDark ? 'dark' : 'light'}, wanted ${shot.theme}`)
    await page.screenshot({ path: new URL(shot.file, OUT).pathname })
    await context.close()
    console.log(`captured ${shot.file} · ${shot.theme} · 1440×900 · ${PATH}`)
  }
} finally {
  await browser.close()
}
