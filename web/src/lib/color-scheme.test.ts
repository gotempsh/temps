// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import path from 'node:path'

const css = readFileSync(
  path.resolve(import.meta.dir, '../globals.css'),
  'utf8'
)

const ruleBody = (selector: string) =>
  css.match(new RegExp(`(?:^|\\n)${selector} \\{([^}]*)\\}`))?.[1] ?? ''

// index.html declares `light dark` so nothing recolours the console; without
// a rule per theme, native controls and scrollbars would follow the OS
// preference instead of the theme the user picked.
test('native controls follow the active console theme, not the OS', () => {
  expect(ruleBody(':root')).toContain('color-scheme: light;')
  expect(ruleBody('\\.dark')).toContain('color-scheme: dark;')
  // Equal specificity: `.dark` must come later to win on <html class="dark">.
  expect(css.indexOf('\n.dark {')).toBeGreaterThan(css.indexOf('\n:root {'))
})
