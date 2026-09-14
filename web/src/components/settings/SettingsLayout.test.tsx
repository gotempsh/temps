// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { SettingsLayout } from './SettingsLayout'

test('settings use the full content width with responsive page gutters', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter initialEntries={['/settings/plugins']}>
      <Routes>
        <Route path="/settings" element={<SettingsLayout />}>
          <Route path="plugins" element={<h1>Plugins</h1>} />
        </Route>
      </Routes>
    </MemoryRouter>
  )

  expect(markup).toContain('<h1>Plugins</h1>')
  expect(markup).toContain('px-4 py-6 sm:px-6 lg:px-8')
  expect(markup).toContain('w-full min-w-0 space-y-6')
  expect(markup).not.toContain('max-w-')
  expect(markup).not.toContain('mx-auto')
})
