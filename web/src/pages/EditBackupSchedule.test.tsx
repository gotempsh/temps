// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
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
