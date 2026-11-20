// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import path from 'node:path'

const css = readFileSync(
  path.resolve(import.meta.dir, '../globals.css'),
  'utf8'
)

// Radix parks the form-bubbling checkbox of a Switch/Checkbox/Radio with
// `translateX(-100%)`; at the start of a flex row that is left of the
// viewport and every form page then reports a control outside the screen.
test('the Radix bubble input of form controls is given no box', () => {
  const rule = css.match(
    /:where\(button\[role='switch'\][^{]*\)\s*\+\s*input\[aria-hidden='true'\]\[tabindex='-1'\]\s*\{([^}]*)\}/
  )
  expect(rule).not.toBeNull()
  expect(rule?.[1]).toContain('width: 0 !important')
  expect(rule?.[1]).toContain('height: 0 !important')
  expect(rule?.[1]).toContain('transform: none !important')
})
