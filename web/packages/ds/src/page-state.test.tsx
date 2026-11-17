// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { Clock3 } from 'lucide-react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { installBrowserWindow } from '../../../src/test/dom'
import { PageState } from './page-state'

installBrowserWindow()
afterEach(cleanup)

test('the not-set-up action navigates inside the app, without a reload', () => {
  const view = render(
    <MemoryRouter initialEntries={['/cron-jobs']}>
      <Routes>
        <Route
          path="/cron-jobs"
          element={
            <PageState
              variant="not-set-up"
              icon={Clock3}
              title="Monitor your scheduled jobs"
              requirement="No cron monitors are configured for this project."
              example="Alert when a daily job never starts."
              settingsHref="/cron-jobs/new"
              settingsLabel="Create monitor"
            />
          }
        />
        <Route path="/cron-jobs/new" element={<p>New monitor form</p>} />
      </Routes>
    </MemoryRouter>
  )
  const link = view.getByRole('link', { name: 'Create monitor' })
  expect(link.getAttribute('href')).toBe('/cron-jobs/new')
  // A plain `<a>` leaves the router where it was; a router link moves it.
  fireEvent.click(link)
  expect(view.getByText('New monitor form')).toBeTruthy()
})
