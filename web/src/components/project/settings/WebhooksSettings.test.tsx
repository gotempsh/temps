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
import type { ProjectResponse } from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { WebhooksSettings } from './WebhooksSettings'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 7, slug: 'demo', name: 'demo' } as ProjectResponse

const WEBHOOK = {
  id: 3,
  project_id: 7,
  url: 'https://hooks.example.test/endpoint',
  events: ['deployment_succeeded'],
  enabled: true,
  has_secret: false,
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}

function renderWebhooks() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/projects/demo/settings/webhooks']}>
        <Routes>
          <Route
            path="/projects/demo/settings/webhooks"
            element={<WebhooksSettings project={PROJECT} />}
          />
          <Route
            path="/projects/demo/settings/webhooks/:id"
            element={<p>webhook detail page</p>}
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('choosing Delete keeps the confirmation open instead of opening the webhook', async () => {
  const api = mockApi({
    'GET /projects/7/webhooks': () => jsonResponse([WEBHOOK]),
    'DELETE /projects/7/webhooks/3': () => jsonResponse({}),
  })
  const view = renderWebhooks()
  const trigger = await view.findByLabelText('Open menu')
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
  await act(async () => {
    fireEvent.click(await view.findByText('Delete'))
  })

  expect(await view.findByText('Delete webhook?')).toBeTruthy()
  // The card's own click handler (open the webhook) must not have fired.
  expect(view.queryByText('webhook detail page')).toBeNull()
  expect(api.called('DELETE', '/projects/7/webhooks/3')).toBe(false)

  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() =>
    expect(api.called('DELETE', '/projects/7/webhooks/3')).toBe(true)
  )
  api.restore()
})
