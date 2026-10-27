// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ShieldPlatformSettings } from './ShieldPlatformSettings'

installBrowserWindow()
afterEach(cleanup)

test('turning fleet learning off saves it at once', async () => {
  let sentBody: unknown = null
  const api = mockApi({
    'GET /settings/shield': () => jsonResponse({ share_observations: true }),
    'PUT /settings/shield': async (request) => {
      sentBody = await request.json()
      return jsonResponse({ share_observations: false })
    },
    'GET /settings/shield/entries': () => jsonResponse([]),
    'GET /settings/shield/proposals': () =>
      jsonResponse({ items: [], total: 3, page: 1, page_size: 1 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ShieldPlatformSettings />
      </MemoryRouter>
    </QueryClientProvider>
  )

  const toggle = await view.findByRole('switch', {
    name: 'Learn from attacks across all services',
  })
  expect(toggle.getAttribute('aria-checked')).toBe('true')
  fireEvent.click(toggle)

  await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('false'))
  expect(sentBody).toEqual({ share_observations: false })
  await view.findByText('No operator entries yet')
  // The proposals are decided on their own page; here only the count.
  await view.findByText(/^3 proposals wait for review/)
  expect(
    view.getByRole('link', { name: 'Review proposals' }).getAttribute('href')
  ).toBe('/settings/shield-proposals')
  api.restore()
})
