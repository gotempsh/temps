// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { NOTHING_TO_BACK_UP_MESSAGE } from '@/lib/backup-coverage'
import { shouldRetryQuery } from '@/lib/query-retry'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { EditBackupSchedule } from './EditBackupSchedule'

installBrowserWindow()
afterEach(cleanup)

function renderAt(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: shouldRetryQuery } },
  })
  return render(
    <QueryClientProvider client={client}>
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={[path]}>
          <Routes>
            <Route
              path="/backups/s3-sources/:id/schedules/:scheduleId/edit"
              element={<EditBackupSchedule />}
            />
          </Routes>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
}

const NOT_FOUND = { title: 'Not Found', detail: 'Schedule 999 not found' }

test('an unknown schedule id shows a not-found state instead of loading forever', async () => {
  const api = mockApi({
    'GET /backups/s3-sources/4': () => jsonResponse({ id: 4, name: 'Archive' }),
    'GET /backups/schedules/999': () => jsonResponse(NOT_FOUND, 404),
    'GET /backups/schedules/999/services': () => jsonResponse(NOT_FOUND, 404),
  })
  const view = renderAt('/backups/s3-sources/4/schedules/999/edit')
  expect(await view.findByText('Schedule not found')).toBeTruthy()
  expect(view.queryByText('Save changes')).toBeNull()
  api.restore()
})

test('a schedule read that fails is reported with a retry, not an endless skeleton', async () => {
  const api = mockApi({
    'GET /backups/s3-sources/4': () => jsonResponse({ id: 4, name: 'Archive' }),
    'GET /backups/schedules/12': () =>
      jsonResponse({ title: 'Forbidden', detail: 'no access' }, 403),
    'GET /backups/schedules/12/services': () =>
      jsonResponse({ title: 'Forbidden', detail: 'no access' }, 403),
  })
  const view = renderAt('/backups/s3-sources/4/schedules/12/edit')
  expect(await view.findByRole('button', { name: /retry/i })).toBeTruthy()
  api.restore()
})

function scheduleBody(overrides: Record<string, unknown>) {
  return {
    id: 12,
    name: 'Nightly',
    backup_type: 'full',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    enabled: true,
    retention_period: 7,
    s3_source_id: 4,
    schedule_expression: '0 0 2 * * *',
    tags: [],
    target_all_services: true,
    include_control_plane: true,
    ...overrides,
  }
}

/** Mocks schedule 12 with `attached` databases; records PATCH bodies. */
function mockSchedule(
  overrides: Record<string, unknown>,
  attached: { id: number; name: string; service_type: string }[] = []
) {
  const patches: Record<string, unknown>[] = []
  const api = mockApi({
    'GET /backups/s3-sources/4': () => jsonResponse({ id: 4, name: 'Archive' }),
    'GET /backups/schedules/12': () => jsonResponse(scheduleBody(overrides)),
    'GET /backups/schedules/12/services': () => jsonResponse(attached),
    'PATCH /backups/schedules/12': async (request) => {
      patches.push((await request.json()) as Record<string, unknown>)
      return jsonResponse(scheduleBody(overrides))
    },
  })
  return { api, patches }
}

async function openEdit() {
  const view = renderAt('/backups/s3-sources/4/schedules/12/edit')
  const save = await view.findByRole('button', { name: 'Save changes' })
  await waitFor(() => expect(save.hasAttribute('disabled')).toBe(false))
  return { view, save }
}

test('a schedule with no databases and the control plane opens as "Control plane only"', async () => {
  const { api, patches } = mockSchedule({
    target_all_services: false,
    include_control_plane: true,
  })
  const { view, save } = await openEdit()

  expect(
    view.getByLabelText('Control plane only').getAttribute('aria-checked')
  ).toBe('true')
  fireEvent.change(view.container.querySelector('#edit-name')!, {
    target: { value: 'Hourly control plane' },
  })
  fireEvent.click(save)

  // Saving it unchanged in coverage writes only the renamed field.
  await waitFor(() => expect(patches).toHaveLength(1))
  expect(patches[0]).toEqual({ name: 'Hourly control plane' })
  api.restore()
})

test('an all-databases schedule can be switched to control plane only', async () => {
  const { api, patches } = mockSchedule({})
  const { view, save } = await openEdit()

  fireEvent.click(view.getByLabelText('Control plane only'))
  fireEvent.click(save)

  await waitFor(() => expect(patches).toHaveLength(1))
  expect(patches[0]).toEqual({ target_all_services: false, service_ids: [] })
  api.restore()
})

test('switching a schedule to control plane only drops its databases', async () => {
  const { api, patches } = mockSchedule(
    { target_all_services: false, include_control_plane: false },
    [{ id: 7, name: 'app-db', service_type: 'postgres' }]
  )
  const { view, save } = await openEdit()

  expect(
    view.getByLabelText('Specific databases').getAttribute('aria-checked')
  ).toBe('true')
  fireEvent.click(view.getByLabelText('Control plane only'))
  fireEvent.click(save)

  await waitFor(() => expect(patches).toHaveLength(1))
  expect(patches[0]).toEqual({ include_control_plane: true, service_ids: [] })
  api.restore()
})

test('only a schedule that would back up nothing is refused, next to the picker', async () => {
  const { api, patches } = mockSchedule({
    target_all_services: false,
    include_control_plane: true,
  })
  const { view, save } = await openEdit()

  fireEvent.click(view.getByLabelText('Specific databases'))
  fireEvent.click(view.getByLabelText('Also back up the Temps control plane'))
  fireEvent.click(save)

  const group = view.container.querySelector<HTMLElement>(
    '#edit-schedule-services'
  )!
  expect(group.getAttribute('aria-invalid')).toBe('true')
  expect(
    document.getElementById(group.getAttribute('aria-describedby')!)
      ?.textContent
  ).toBe(NOTHING_TO_BACK_UP_MESSAGE)
  expect(document.activeElement).toBe(group)
  expect(patches).toHaveLength(0)
  expect(api.calls.some((call) => call.method === 'PATCH')).toBe(false)
  api.restore()
})
