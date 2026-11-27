// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { ErrorGroupAutofixRedirect } from './ErrorGroupAutofixRedirect'

installBrowserWindow()
afterEach(cleanup)

function Where() {
  const { pathname, search, hash } = useLocation()
  return <output data-testid="where">{`${pathname}${search}${hash}`}</output>
}

test('the old Fix with AI address opens the error group, keeping its query and hash', () => {
  const view = render(
    <MemoryRouter initialEntries={['/projects/web/errors/5/autofix?a=b#c']}>
      <Routes>
        <Route path="/projects/web">
          <Route
            path="errors/:errorGroupId/autofix"
            element={<ErrorGroupAutofixRedirect />}
          />
          <Route path="errors/:errorGroupId" element={<Where />} />
        </Route>
      </Routes>
    </MemoryRouter>
  )
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/errors/5?a=b#c'
  )
})
