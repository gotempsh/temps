// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { RequestLogsRedirect } from './RequestLogsRedirect'

installBrowserWindow()
afterEach(cleanup)

function Where() {
  const { pathname, search, hash } = useLocation()
  return <output data-testid="where">{`${pathname}${search}${hash}`}</output>
}

function renderAt(entry: string) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <Routes>
        <Route
          path="/projects/web/analytics/requests/*"
          element={<RequestLogsRedirect projectSlug="web" />}
        />
        <Route path="/projects/web/request-logs" element={<Where />} />
        <Route path="/projects/web/request-logs/:id" element={<Where />} />
      </Routes>
    </MemoryRouter>
  )
}

test('the analytics request-log list keeps the query string and hash', () => {
  const view = renderAt('/projects/web/analytics/requests?a=b#c')
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/request-logs?a=b#c'
  )
})

test('an analytics request-log detail keeps its id, query string and hash', () => {
  const view = renderAt('/projects/web/analytics/requests/42?a=b#c')
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/request-logs/42?a=b#c'
  )
})
