// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { renderToStaticMarkup } from 'react-dom/server'
import { InitialLoadScreen } from './InitialLoadScreen'

test('the first-paint screen announces itself and carries both animations', () => {
  const html = renderToStaticMarkup(<InitialLoadScreen />)
  expect(html).toContain('role="status"')
  expect(html).toContain('aria-label="Loading application"')
  expect(html).toContain('initial-load-mark')
  expect(html).toContain('initial-load-bar')
})

test('the animations exist and reduced motion keeps a fade rather than going static', () => {
  const css = readFileSync(
    new URL('../../globals.css', import.meta.url),
    'utf8'
  )
  expect(css).toMatch(
    /\.initial-load-bar\s*{[^}]*animation:\s*initial-load-slide/
  )
  expect(css).toMatch(
    /\.initial-load-mark\s*{[^}]*animation:\s*initial-load-pulse/
  )
  const reduced = css.slice(
    css.indexOf(
      'prefers-reduced-motion: reduce',
      css.indexOf('.initial-load-bar')
    )
  )
  expect(reduced).toContain('initial-load-pulse')
})
