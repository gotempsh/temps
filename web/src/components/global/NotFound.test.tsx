// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import NotFound from './NotFound'

test('the dark-theme message is light enough for 4.5:1 on the page background', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <NotFound />
    </MemoryRouter>
  )
  const message = /<p class="([^"]*)">/.exec(html)?.[1] ?? ''
  // gray-500 on #0a0a0a is 4.09:1 (axe `color-contrast`); gray-400 is 7.6:1.
  expect(message).toContain('dark:text-gray-400')
  expect(message).not.toContain('dark:text-gray-500')
})
