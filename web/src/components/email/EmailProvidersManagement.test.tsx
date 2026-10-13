// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { EmailProvidersManagement } from './EmailProvidersManagement'

installBrowserWindow()
afterEach(cleanup)

const PROVIDER = {
  id: 5,
  name: 'Primary relay',
  provider_type: 'smtp',
  region: 'eu-test-1',
  is_active: true,
  credentials: {},
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}

function renderProviders() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/email']}>
        <Routes>
          <Route path="/email" element={<EmailProvidersManagement />} />
          <Route path="/email/providers/:id" element={<p>provider detail</p>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

async function openDeleteDialog(view: ReturnType<typeof render>) {
  const trigger = await view.findByLabelText('Open menu')
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
  await act(async () => {
    fireEvent.click(await view.findByText('Delete'))
  })
}

test('deleting an email provider asks first, naming it, and sends nothing until confirmed', async () => {
  const api = mockApi({
    'GET /email-providers': () => jsonResponse([PROVIDER]),
    'DELETE /email-providers/5': () => jsonResponse({}),
  })
  const view = renderProviders()
  await openDeleteDialog(view)

  expect(await view.findByText(`Delete ${PROVIDER.name}?`)).toBeTruthy()
  expect(api.called('DELETE', '/email-providers/5')).toBe(false)

  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  })
  expect(api.called('DELETE', '/email-providers/5')).toBe(false)
  // Interacting with the dialog must not open the provider's detail page.
  expect(view.queryByText('provider detail')).toBeNull()

  await openDeleteDialog(view)
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() =>
    expect(api.called('DELETE', '/email-providers/5')).toBe(true)
  )
  expect(view.queryByText('provider detail')).toBeNull()
  api.restore()
})
