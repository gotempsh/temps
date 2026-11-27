// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, Route, Routes } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { LegacyAutofixNotice } from './LegacyAutofixNotice'

installBrowserWindow()
afterEach(cleanup)

test('an old autofixer run links to its error group to start an incident', () => {
  const view = render(
    <MemoryRouter initialEntries={['/projects/web/agents/9']}>
      <Routes>
        <Route path="/projects/web">
          <Route
            path="agents/:runId"
            element={<LegacyAutofixNotice errorGroupId={12} />}
          />
        </Route>
      </Routes>
    </MemoryRouter>
  )
  expect(view.getByText('Autofix now runs as an incident')).toBeTruthy()
  const link = view.getByRole('link', { name: 'Open the error group' })
  expect(link.getAttribute('href')).toBe('/projects/web/errors/12')
})

test('without an error group there is nothing to link to', () => {
  const view = render(
    <MemoryRouter>
      <LegacyAutofixNotice errorGroupId={null} />
    </MemoryRouter>
  )
  expect(view.queryByRole('link')).toBeNull()
})
