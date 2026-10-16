// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { CATEGORY_META } from './AuditLogItem-shared'

// Colour -600 on a 10% tint of itself is below the 4.5:1 WCAG 1.4.3 minimum
// for several hues (lime measured 2.85:1). Light-theme text must be -700 or
// darker; the dark theme has its own lighter text.
test('audit category badges use a text shade that meets contrast on the tint', () => {
  const weak = Object.entries(CATEGORY_META)
    .filter(([, meta]) => meta.tone.includes('/10'))
    .filter(([, meta]) =>
      /(^|\s)text-[a-z]+-(50|100|200|300|400|500|600)(\s|$)/.test(meta.tone)
    )
    .map(([category, meta]) => `${category}: ${meta.tone}`)
  expect(weak).toEqual([])
})
