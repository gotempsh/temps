// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NOTHING_TO_BACK_UP_MESSAGE } from '@/lib/backup-coverage'
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
  fireEvent.click(view.getByLabelText('Also back up the Temps control plane'))
  fireEvent.change(byId(view, 'max-runtime'), { target: { value: '0' } })
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  for (const [id, message] of [
    ['name', 'Schedule name is required'],
    ['custom-cron', 'Enter a custom cron expression'],
    ['schedule-services', NOTHING_TO_BACK_UP_MESSAGE],
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
  fireEvent.click(view.getByLabelText('Also back up the Temps control plane'))
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  fireEvent.change(byId(view, 'name'), { target: { value: 'Nightly' } })

  expect(byId(view, 'name').getAttribute('aria-invalid')).toBeNull()
  expect(view.queryByText('Schedule name is required')).toBeNull()
  expect(byId(view, 'schedule-services').getAttribute('aria-invalid')).toBe(
    'true'
  )
  api.restore()
})

/** Mocks the source and records the body of the create request. */
function mockCreate() {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    'GET /backups/s3-sources/4': () => jsonResponse({ id: 4, name: 'Archive' }),
    'POST /backups/schedules': async (request) => {
      bodies.push((await request.json()) as Record<string, unknown>)
      return jsonResponse({ id: 31, name: 'Control plane hourly' })
    },
  })
  return { api, bodies }
}

test('"Control plane only" creates a schedule with no databases', async () => {
  const { api, bodies } = mockCreate()
  const view = renderPage()

  fireEvent.change(byId(view, 'name'), {
    target: { value: 'Control plane hourly' },
  })
  fireEvent.click(view.getByLabelText('Control plane only'))
  // The option is the control plane; there is no database picker or switch.
  expect(view.container.querySelector('#schedule-services')).toBeNull()
  expect(view.container.querySelector('#include-control-plane')).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  await waitFor(() => expect(bodies).toHaveLength(1))
  expect(bodies[0]).toMatchObject({
    name: 'Control plane hourly',
    target_all_services: false,
    include_control_plane: true,
    service_ids: [],
  })
  expect(view.queryByText(NOTHING_TO_BACK_UP_MESSAGE)).toBeNull()
  api.restore()
})

test('specific databases with none picked is valid while the control plane is on', async () => {
  const { api, bodies } = mockCreate()
  const view = renderPage()

  fireEvent.change(byId(view, 'name'), { target: { value: 'Nightly' } })
  fireEvent.click(view.getByLabelText('Specific databases'))
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))

  await waitFor(() => expect(bodies).toHaveLength(1))
  expect(bodies[0]).toMatchObject({
    target_all_services: false,
    include_control_plane: true,
    service_ids: [],
  })
  expect(view.queryByText(NOTHING_TO_BACK_UP_MESSAGE)).toBeNull()
  api.restore()
})

test('turning the control plane back on clears the nothing-to-back-up message', () => {
  const { api, bodies } = mockCreate()
  const view = renderPage()

  fireEvent.change(byId(view, 'name'), { target: { value: 'Nightly' } })
  fireEvent.click(view.getByLabelText('Specific databases'))
  const controlPlane = view.getByLabelText(
    'Also back up the Temps control plane'
  )
  fireEvent.click(controlPlane)
  fireEvent.click(view.getByRole('button', { name: 'Create schedule' }))
  expect(view.getByText(NOTHING_TO_BACK_UP_MESSAGE)).toBeTruthy()
  expect(document.activeElement).toBe(byId(view, 'schedule-services'))

  fireEvent.click(controlPlane)
  expect(view.queryByText(NOTHING_TO_BACK_UP_MESSAGE)).toBeNull()
  expect(bodies).toHaveLength(0)
  api.restore()
})

test('"All databases" stays the default choice', () => {
  const api = mockSource()
  const view = renderPage()

  const all = view.getByLabelText('All databases (recommended)')
  expect(all.getAttribute('aria-checked')).toBe('true')
  expect(
    view.getByLabelText('Control plane only').getAttribute('aria-checked')
  ).toBe('false')
  api.restore()
})
