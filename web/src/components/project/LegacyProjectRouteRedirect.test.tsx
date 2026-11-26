// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { RenamedProjectRouteRedirect } from './LegacyProjectRouteRedirect'

installBrowserWindow()
afterEach(cleanup)

function Shown() {
  const { pathname, search } = useLocation()
  return <p data-testid="shown">{`${pathname}${search}`}</p>
}

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/projects/:slug">
          <Route
            path="observe/*"
            element={
              <RenamedProjectRouteRedirect
                projectSlug="web"
                from="observe"
                to="activity"
              />
            }
          />
          <Route path="*" element={<Shown />} />
        </Route>
      </Routes>
    </MemoryRouter>
  )
}

test("a bookmark to a page's former URL lands on its page, filters kept", () => {
  const view = renderAt('/projects/web/observe?env=staging&type=deploy')
  expect(view.getByTestId('shown').textContent).toBe(
    '/projects/web/activity?env=staging&type=deploy'
  )
})
