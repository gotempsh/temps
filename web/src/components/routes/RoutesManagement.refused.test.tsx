// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { Toaster, toast } from 'sonner'
import type { RouteResponse } from '@/api/client'
import { createAppQueryClient } from '@/test/app-query-client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { RoutesManagement } from './RoutesManagement'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const ROUTE: RouteResponse = {
  id: 2,
  domain: 'app.example.com',
  host: '10.0.0.5',
  port: 8080,
  enabled: true,
  route_type: 'http',
  created_at: 0,
  updated_at: 0,
}

function renderRoutes(reloadRoutes: () => void) {
  const client = createAppQueryClient()
  return render(
    <QueryClientProvider client={client}>
      <RoutesManagement
        routes={[ROUTE]}
        isLoading={false}
        reloadRoutes={reloadRoutes}
      />
      <Toaster />
    </QueryClientProvider>
  )
}

async function deleteRow(view: ReturnType<typeof render>) {
  const trigger = view.getByRole('button', { name: 'Open menu' })
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
  await act(async () => {
    fireEvent.click(await view.findByText('Delete'))
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
}

test('a refused route delete is reported, not announced as deleted', async () => {
  let reloads = 0
  const api = mockApi({
    'DELETE /lb/routes/app.example.com': () =>
      jsonResponse(
        {
          title: 'Forbidden',
          status: 403,
          detail: 'Requires load_balancer:write permission',
        },
        403
      ),
  })
  const view = renderRoutes(() => (reloads += 1))
  await deleteRow(view)

  await waitFor(() =>
    expect(api.called('DELETE', '/lb/routes/app.example.com')).toBe(true)
  )
  expect(await view.findByText('Failed to delete route')).toBeTruthy()
  expect(
    await view.findByText('Requires load_balancer:write permission')
  ).toBeTruthy()
  expect(view.queryByText('Route deleted successfully')).toBeNull()
  expect(reloads).toBe(0)
  api.restore()
})

test('an accepted route delete is announced and reloads the list', async () => {
  let reloads = 0
  const api = mockApi({
    'DELETE /lb/routes/app.example.com': () =>
      new Response(null, { status: 204 }),
  })
  const view = renderRoutes(() => (reloads += 1))
  await deleteRow(view)

  expect(await view.findByText('Route deleted successfully')).toBeTruthy()
  expect(reloads).toBe(1)
  api.restore()
})
