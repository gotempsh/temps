// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import type { FunnelResponse, ProjectResponse } from '@/api/client'
import { getFunnelQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { FunnelManagement } from './FunnelManagement'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 3, slug: 'shop' } as ProjectResponse

const FUNNEL: FunnelResponse = {
  id: 7,
  project_id: 3,
  name: 'Checkout',
  description: null,
  is_active: true,
  steps: [{ id: 1, order: 1, event_name: 'page_view', event_filter: [] }],
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

test('deleting a funnel refetches the list and forgets the funnel', async () => {
  let deleted = false
  let listReads = 0
  const api = mockApi({
    'GET /projects/3/funnels': () => {
      listReads += 1
      return jsonResponse({
        items: deleted ? [] : [FUNNEL],
        total: deleted ? 0 : 1,
        page: 1,
        page_size: 12,
      })
    },
    'GET /projects/3/funnels/7/metrics': () =>
      jsonResponse({
        funnel_id: 7,
        funnel_name: 'Checkout',
        total_entries: 0,
        step_conversions: [],
        overall_conversion_rate: 0,
        average_completion_time_seconds: 0,
      }),
    'DELETE /projects/3/funnels/7': () => {
      deleted = true
      return jsonResponse({ message: 'Funnel deleted successfully' })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const funnelKey = getFunnelQueryKey({ path: { project_id: 3, funnel_id: 7 } })
  client.setQueryData(funnelKey, FUNNEL)
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <FunnelManagement project={PROJECT} />
      </MemoryRouter>
    </QueryClientProvider>
  )

  await act(async () => {
    fireEvent.click(await view.findByLabelText('Delete funnel'))
  })
  const readsBeforeDelete = listReads
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Delete' }))
  })

  await waitFor(() => expect(deleted).toBe(true))
  // The list was asked again (a bare ['listFunnels'] key never matched it).
  await waitFor(() => expect(listReads).toBeGreaterThan(readsBeforeDelete))
  expect(client.getQueryState(funnelKey)).toBeUndefined()
  api.restore()
})
