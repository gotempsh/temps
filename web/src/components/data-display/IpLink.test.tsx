// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { IpLink } from './IpLink'

installBrowserWindow()
afterEach(cleanup)

test('links an address to its IP page, IPv6 as one path segment', () => {
  const view = render(
    <MemoryRouter>
      <IpLink ip="203.0.113.7" />
      <IpLink ip="2001:db8::1" />
    </MemoryRouter>
  )
  expect(
    view.getByRole('link', { name: '203.0.113.7' }).getAttribute('href')
  ).toBe('/ip/203.0.113.7')
  expect(
    view.getByRole('link', { name: '2001:db8::1' }).getAttribute('href')
  ).toBe('/ip/2001%3Adb8%3A%3A1')
})

test('opens the address, not the clickable row it sits in', () => {
  let rowClicks = 0
  const view = render(
    <MemoryRouter initialEntries={['/logs']}>
      <Routes>
        <Route
          path="/logs"
          element={
            <div onClick={() => rowClicks++}>
              <IpLink ip="203.0.113.7" />
            </div>
          }
        />
        <Route path="/ip/:ip" element={<p>IP page</p>} />
      </Routes>
    </MemoryRouter>
  )
  fireEvent.click(view.getByRole('link', { name: '203.0.113.7' }))
  expect(rowClicks).toBe(0)
  expect(view.getByText('IP page')).toBeTruthy()
})
