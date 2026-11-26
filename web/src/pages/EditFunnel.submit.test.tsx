// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import type { FunnelResponse, ProjectResponse } from '@/api/client'
import { getFunnelQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EditFunnel } from './EditFunnel'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 3, slug: 'shop' } as ProjectResponse

function funnel(name: string): FunnelResponse {
  return {
    id: 41,
    project_id: 3,
    name,
    description: null,
    is_active: true,
    steps: [
      { id: 1, order: 1, event_name: 'checkout_started', event_filter: [] },
    ],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }
}

test('saving an edit puts the updated funnel in the cache the funnel page reads', async () => {
  const api = mockApi({
    'GET /projects/3/funnels/41': () => jsonResponse(funnel('Checkout')),
    'GET /projects/3/events/unique': () => jsonResponse({ events: [] }),
    'POST /projects/3/funnels/preview': () =>
      jsonResponse({
        funnel_id: 0,
        funnel_name: 'Preview',
        average_completion_time_seconds: 0,
        overall_conversion_rate: 0,
        step_conversions: [],
        total_entries: 0,
      }),
    'PUT /projects/3/funnels/41': () => jsonResponse(funnel('Checkout v2')),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter
        initialEntries={['/projects/shop/analytics/funnels/41/edit']}
      >
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/projects/shop/analytics/funnels/41/edit"
              element={<EditFunnel project={PROJECT} funnelId={41} />}
            />
            <Route
              path="/projects/shop/analytics/funnels/41"
              element={<p>Funnel opened</p>}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )

  const name = await view.findByLabelText('Funnel Name *')
  await act(async () => {
    fireEvent.change(name, { target: { value: 'Checkout v2' } })
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Update Funnel' }))
  })
  expect(await view.findByText('Funnel updated successfully!')).toBeTruthy()

  // The funnel page reads this key; it must show the saved funnel, not the
  // one loaded before the edit.
  const cached = client.getQueryData<FunnelResponse>(
    getFunnelQueryKey({ path: { project_id: 3, funnel_id: 41 } })
  )
  expect(cached?.name).toBe('Checkout v2')
  api.restore()
})
