// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { FirstProjectOnboarding } from './FirstProjectOnboarding'

test('empty installs choose a feature before opening its existing setup flow', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <FirstProjectOnboarding />
    </MemoryRouter>
  )
  expect(markup).toContain('What would you like to try first?')
  expect(markup).toContain('href="/projects/new?source=templates"')
  expect(markup).toContain('href="/sandboxes?new=sandbox"')
  expect(markup).toContain('Send email')
  expect(markup).toContain('Monitor an app')
  expect(markup).not.toContain('Deploy sample app')
})
