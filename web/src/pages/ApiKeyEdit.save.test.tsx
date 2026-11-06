// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
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
import ApiKeyEdit from './ApiKeyEdit'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

// Expires mid-afternoon, a year from the test date: a date input shows only
// the day, so saving without touching it must not move the time.
const STORED_EXPIRY = '2027-11-05T15:45:00Z'

const KEY = {
  id: 1,
  name: 'ci-deploy-key',
  key_prefix: 'tk_abc',
  role_type: 'admin',
  permissions: null,
  is_active: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  expires_at: STORED_EXPIRY,
  last_used_at: null,
}

function renderPage() {
  const client = createAppQueryClient()
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/keys/1/edit']}>
        <Routes>
          <Route path="/settings/keys/:id/edit" element={<ApiKeyEdit />} />
          <Route path="/settings/keys" element={<p>API key list</p>} />
        </Routes>
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

async function rename(view: ReturnType<typeof render>, name: string) {
  const input = await view.findByLabelText('Name')
  await act(async () => {
    fireEvent.change(input, { target: { value: name } })
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save' }))
  })
}

test('a save is sent as the PATCH the server routes and keeps the stored expiry', async () => {
  let sent: Record<string, unknown> | null = null
  const api = mockApi({
    'GET /api-keys/1': () => jsonResponse(KEY),
    'PATCH /api-keys/1': async (request) => {
      sent = (await request.json()) as Record<string, unknown>
      return jsonResponse({ ...KEY, name: 'renamed-key' })
    },
  })
  const view = renderPage()
  await rename(view, 'renamed-key')

  expect(await view.findByText('API key updated successfully')).toBeTruthy()
  expect(api.called('PUT', '/api-keys/1')).toBe(false)
  expect(sent).not.toBeNull()
  expect(sent!.name).toBe('renamed-key')
  expect(sent!.expires_at).toBe(STORED_EXPIRY)
  api.restore()
})

test('a refused save is reported with the reason and stays on the form', async () => {
  const api = mockApi({
    'GET /api-keys/1': () => jsonResponse(KEY),
    'PATCH /api-keys/1': () =>
      jsonResponse(
        {
          title: 'Bad Request',
          status: 400,
          detail: 'API key name ci-deploy-key is already in use',
        },
        400
      ),
  })
  const view = renderPage()
  await rename(view, 'taken-name')

  await waitFor(() => expect(api.called('PATCH', '/api-keys/1')).toBe(true))
  expect(await view.findByText('Failed to update API key')).toBeTruthy()
  expect(
    await view.findByText('API key name ci-deploy-key is already in use')
  ).toBeTruthy()
  expect(view.queryByText('API key updated successfully')).toBeNull()
  expect(view.queryByText('API key list')).toBeNull()
  api.restore()
})

test('clearing a stored expiry is refused instead of reporting a save that changes nothing', async () => {
  const api = mockApi({
    'GET /api-keys/1': () => jsonResponse(KEY),
    'PATCH /api-keys/1': () => jsonResponse(KEY),
  })
  const view = renderPage()
  const expiry = await view.findByLabelText(/Expiration date/)
  expect(expiry.hasAttribute('required')).toBe(true)
  expect(view.getByText(/an expiration date cannot be removed/)).toBeTruthy()

  await act(async () => {
    fireEvent.change(expiry, { target: { value: '' } })
  })
  // Submitted directly: a browser's own `required` check would stop the
  // click first.
  await act(async () => {
    fireEvent.submit(expiry.closest('form')!)
  })

  expect(
    await view.findByText('The expiration date cannot be removed')
  ).toBeTruthy()
  expect(api.called('PATCH', '/api-keys/1')).toBe(false)
  expect(view.queryByText('API key updated successfully')).toBeNull()
  expect(view.queryByText('API key list')).toBeNull()
  api.restore()
})

test('a key that never expires can still be saved without an expiry', async () => {
  let sent: Record<string, unknown> | null = null
  const api = mockApi({
    'GET /api-keys/1': () => jsonResponse({ ...KEY, expires_at: null }),
    'PATCH /api-keys/1': async (request) => {
      sent = (await request.json()) as Record<string, unknown>
      return jsonResponse({ ...KEY, expires_at: null, name: 'renamed-key' })
    },
  })
  const view = renderPage()
  const expiry = await view.findByLabelText(/Expiration date/)
  expect(expiry.hasAttribute('required')).toBe(false)
  await rename(view, 'renamed-key')

  expect(await view.findByText('API key updated successfully')).toBeTruthy()
  expect(sent).not.toBeNull()
  expect(sent!.expires_at ?? null).toBeNull()
  api.restore()
})
