// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CreateBackupSchedule } from './CreateBackupSchedule'

installBrowserWindow()
afterEach(cleanup)

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/backups/s3-sources/4/schedules/new']}>
          <Routes>
            <Route
              path="/backups/s3-sources/:id/schedules/new"
              element={<CreateBackupSchedule />}
            />
          </Routes>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
}

function mockSource() {
  return mockApi({
    'GET /backups/s3-sources/4': () => jsonResponse({ id: 4, name: 'Archive' }),
  })
}

function byId(view: ReturnType<typeof renderPage>, id: string) {
  return view.container.querySelector<HTMLElement>(`#${id}`)!
}

test('an empty create marks the name invalid with a message and focuses it', () => {
  const api = mockSource()
  const view = renderPage()

  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  const name = byId(view, 'name')
  expect(name.getAttribute('aria-invalid')).toBe('true')
  const message = view.getByText('Schedule name is required')
  expect(name.getAttribute('aria-describedby')).toBe(message.id)
  expect(document.activeElement).toBe(name)
  expect(api.calls.some((call) => call.method === 'POST')).toBe(false)
  api.restore()
})

test('every problem is reported on the same click', () => {
  const api = mockSource()
  const view = renderPage()

  fireEvent.click(view.getByLabelText(/Custom/))
  fireEvent.click(view.getByLabelText('Specific databases'))
  fireEvent.change(byId(view, 'max-runtime'), { target: { value: '0' } })
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  for (const [id, message] of [
    ['name', 'Schedule name is required'],
    ['custom-cron', 'Enter a custom cron expression'],
    [
      'schedule-services',
      'Select at least one database, or switch back to "All databases."',
    ],
    ['max-runtime', 'Max runtime must be a positive number of hours'],
  ] as const) {
    const field = byId(view, id)
    expect(field.getAttribute('aria-invalid')).toBe('true')
    const described = field.getAttribute('aria-describedby')
    expect(document.getElementById(described!)?.textContent).toBe(message)
  }
  expect(document.activeElement).toBe(byId(view, 'name'))
  api.restore()
})

test('typing a name clears only the name message', () => {
  const api = mockSource()
  const view = renderPage()
  fireEvent.click(view.getByLabelText('Specific databases'))
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  fireEvent.change(byId(view, 'name'), { target: { value: 'Nightly' } })

  expect(byId(view, 'name').getAttribute('aria-invalid')).toBeNull()
  expect(view.queryByText('Schedule name is required')).toBeNull()
  expect(byId(view, 'schedule-services').getAttribute('aria-invalid')).toBe(
    'true'
  )
  api.restore()
})
