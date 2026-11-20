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
import type { ProviderKeyResponse } from '@/api/client'
import { createAppQueryClient } from '@/test/app-query-client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { AiGatewayPage } from './AiGateway'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const KEY: ProviderKeyResponse = {
  id: 4,
  provider: 'openai',
  display_name: 'Example OpenAI key',
  api_key_masked: 'sk-...abcd',
  is_active: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

const REFUSED = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires ai_gateway:write permission',
}

/** Key actions live in the provider's row, which opens on click. */
async function expandOpenAi(view: ReturnType<typeof render>) {
  await waitFor(() =>
    expect(view.container.querySelector('tr.cursor-pointer')).not.toBeNull()
  )
  await act(async () => {
    fireEvent.click(view.container.querySelector('tr.cursor-pointer')!)
  })
}

function renderPage() {
  const client = createAppQueryClient()
  client.setQueryData(['platform-settings'], {
    external_url: 'https://temps.example.test',
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/ai-gateway']}>
        <AiGatewayPage />
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a refused provider key delete is reported, not announced as deleted', async () => {
  const api = mockApi({
    'GET /ai/providers': () => jsonResponse([KEY]),
    'DELETE /ai/providers/4': () => jsonResponse(REFUSED, 403),
  })
  const view = renderPage()
  await expandOpenAi(view)
  const remove = await view.findByTitle('Delete key')
  await act(async () => {
    fireEvent.click(remove)
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })

  await waitFor(() =>
    expect(api.called('DELETE', '/ai/providers/4')).toBe(true)
  )
  expect(await view.findByText('Failed to delete provider key')).toBeTruthy()
  expect(await view.findByText(REFUSED.detail)).toBeTruthy()
  expect(view.queryByText('Provider key deleted')).toBeNull()
  api.restore()
})

test('a refused enable/disable is reported', async () => {
  const api = mockApi({
    'GET /ai/providers': () => jsonResponse([KEY]),
    'PATCH /ai/providers/4': () => jsonResponse(REFUSED, 403),
  })
  const view = renderPage()
  await expandOpenAi(view)
  const toggle = await view.findByTitle('Disable key')
  await act(async () => {
    fireEvent.click(toggle)
  })

  expect(await view.findByText('Failed to update provider key')).toBeTruthy()
  expect(await view.findByText(REFUSED.detail)).toBeTruthy()
  api.restore()
})
