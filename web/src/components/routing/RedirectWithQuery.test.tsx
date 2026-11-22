// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { redirectTarget } from '@/lib/redirect-target'
import { RedirectWithQuery } from './RedirectWithQuery'

installBrowserWindow()
afterEach(cleanup)

test('the incoming query string and hash are appended to the target', () => {
  expect(redirectTarget('/projects', '?search=x&tab=y', '#top')).toEqual({
    pathname: '/projects',
    search: '?search=x&tab=y',
    hash: '#top',
  })
  expect(redirectTarget('/projects', '', '')).toEqual({
    pathname: '/projects',
    search: '',
    hash: '',
  })
})

test('parameters on the target win over the incoming ones', () => {
  expect(
    redirectTarget('/projects/new?source=drop', '?source=other&ref=a', '')
  ).toEqual({
    pathname: '/projects/new',
    search: '?source=drop&ref=a',
    hash: '',
  })
})

test('a repeated incoming parameter keeps every value', () => {
  expect(redirectTarget('/logs', '?attr=a&attr=b&level=error', '').search).toBe(
    '?attr=a&attr=b&level=error'
  )
})

test('a target parameter replaces every incoming value of that key', () => {
  expect(
    redirectTarget('/logs?level=warn', '?level=a&level=b&attr=x', '').search
  ).toBe('?level=warn&attr=x')
})

function Where() {
  const location = useLocation()
  return (
    <output data-testid="where">{`${location.pathname}${location.search}`}</output>
  )
}

test('a legacy route lands on its replacement with the query intact', () => {
  const view = render(
    <MemoryRouter initialEntries={['/alarms?search=disk&tab=open']}>
      <Routes>
        <Route
          path="/alarms"
          element={<RedirectWithQuery to="/monitoring/alarms" />}
        />
        <Route path="/monitoring/alarms" element={<Where />} />
      </Routes>
    </MemoryRouter>
  )
  expect(view.getByTestId('where').textContent).toBe(
    '/monitoring/alarms?search=disk&tab=open'
  )
})

test('a relative redirect inside nested routes keeps the query too', () => {
  const view = render(
    <MemoryRouter initialEntries={['/monitoring?search=x']}>
      <Routes>
        <Route path="/monitoring">
          <Route index element={<RedirectWithQuery to="alerts" />} />
          <Route path="alerts" element={<Where />} />
        </Route>
      </Routes>
    </MemoryRouter>
  )
  expect(view.getByTestId('where').textContent).toBe(
    '/monitoring/alerts?search=x'
  )
})
