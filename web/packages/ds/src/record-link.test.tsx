// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { RecordLink } from './record-link'

test('record navigation is a real named link with a permanent affordance', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <RecordLink
        to="/variables/7"
        aria-label="View API_KEY details"
        className="font-mono"
        target="_blank"
        rel="noopener"
      >
        API_KEY
      </RecordLink>
    </MemoryRouter>
  )
  expect(markup).toContain('href="/variables/7"')
  expect(markup).toContain('aria-label="View API_KEY details"')
  expect(markup).toContain('target="_blank"')
  expect(markup).toContain('font-mono')
  expect(markup).toContain(' underline ')
  expect(markup).toContain('focus-visible:outline-2')
  expect(markup).toContain('aria-hidden="true"')
  expect(markup).not.toContain('<button')
  expect(markup).not.toContain('opacity-0')
})
