// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { Toaster, toast } from 'sonner'
import { createAppQueryClient } from '@/test/app-query-client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import ApiKeys from './ApiKeys'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const KEY = {
  id: 9,
  name: 'ci-deploy-key',
  key_prefix: 'tk_abc',
  role_type: 'admin',
  permissions: null,
  is_active: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  expires_at: null,
  last_used_at: null,
}

const REFUSED = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires api_keys:write permission',
}

// The page relies on the app-wide mutation error handler for its failure
// toasts, so the test client installs the same one App.tsx does.
function renderPage() {
  const client = createAppQueryClient()
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/keys']}>
        <ApiKeys />
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

async function openRowMenu(view: ReturnType<typeof render>) {
  const trigger = await view.findByLabelText('Open menu')
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
}

const listRoute = {
  'GET /api-keys': () => jsonResponse({ api_keys: [KEY], total: 1 }),
}

test('a refused delete is reported, not announced as deleted', async () => {
  const api = mockApi({
    ...listRoute,
    'DELETE /api-keys/9': () => jsonResponse(REFUSED, 403),
  })
  const view = renderPage()
  await openRowMenu(view)
  await act(async () => {
    fireEvent.click(await view.findByText('Delete'))
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() => expect(api.called('DELETE', '/api-keys/9')).toBe(true))

  expect(await view.findByText('Failed to delete API key')).toBeTruthy()
  expect(await view.findByText(REFUSED.detail)).toBeTruthy()
  expect(view.queryByText('API key deleted successfully')).toBeNull()
  // The confirmation stays open for a retry.
  expect(view.getByText('Delete API key ci-deploy-key?')).toBeTruthy()
  api.restore()
})

test('a refused deactivate is reported, not announced as deactivated', async () => {
  const api = mockApi({
    ...listRoute,
    'POST /api-keys/9/deactivate': () => jsonResponse(REFUSED, 403),
  })
  const view = renderPage()
  await openRowMenu(view)
  await act(async () => {
    fireEvent.click(await view.findByText('Deactivate'))
  })
  await waitFor(() =>
    expect(api.called('POST', '/api-keys/9/deactivate')).toBe(true)
  )

  expect(await view.findByText('Failed to deactivate API key')).toBeTruthy()
  expect(view.queryByText('API key deactivated')).toBeNull()
  api.restore()
})

test('an accepted delete is announced', async () => {
  const api = mockApi({
    ...listRoute,
    'DELETE /api-keys/9': () => new Response(null, { status: 204 }),
  })
  const view = renderPage()
  await openRowMenu(view)
  await act(async () => {
    fireEvent.click(await view.findByText('Delete'))
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  expect(await view.findByText('API key deleted successfully')).toBeTruthy()
  api.restore()
})
