// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ConversionsCard } from './ConversionsCard'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 7, slug: 'site' } as ProjectResponse
const range = {
  project,
  startDate: new Date('2026-11-12T00:00:00Z'),
  endDate: new Date('2026-11-19T00:00:00Z'),
  environment: undefined,
}

function renderWithClient(node: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>
  )
}

const purchase = {
  id: 3,
  project_id: 7,
  name: 'Purchase',
  kind: 'event',
  event_name: 'purchase',
  event_filter: [],
  page_path: null,
  revenue_property: 'amount',
  revenue_currency: 'USD',
  created_at: '2026-11-19T00:00:00Z',
  updated_at: '2026-11-19T00:00:00Z',
}

test('without goals the card stays and explains how to define one', async () => {
  const api = mockApi({
    'GET /projects/7/analytics/goals': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 100 }),
    'GET /projects/7/analytics/goals/stats': () =>
      jsonResponse({ visitors: 120, goals: [] }),
  })
  const view = renderWithClient(<ConversionsCard {...range} />)

  expect(await view.findByText(/No conversions defined yet/)).toBeTruthy()
  expect(
    view.getByRole('button', { name: 'Define your first conversion' })
  ).toBeTruthy()
  expect(view.getByRole('button', { name: 'Add conversion' })).toBeTruthy()
  api.restore()
})

test('lists conversions with rate and revenue, and breaks one down by channel', async () => {
  const breakdownQueries: URLSearchParams[] = []
  const api = mockApi({
    'GET /projects/7/analytics/goals': () =>
      jsonResponse({ items: [purchase], total: 1, page: 1, page_size: 100 }),
    'GET /projects/7/analytics/goals/stats': () =>
      jsonResponse({
        visitors: 300,
        goals: [
          {
            goal_id: 3,
            name: 'Purchase',
            kind: 'event',
            unique_conversions: 12,
            total_conversions: 15,
            conversion_rate: 4,
            revenue: { currency: 'USD', total: 600, average: 40 },
          },
        ],
      }),
    'GET /projects/7/analytics/goals/3/breakdown': (request) => {
      breakdownQueries.push(new URL(request.url).searchParams)
      return jsonResponse({
        goal_id: 3,
        dimension: 'channel',
        items: [
          {
            value: 'Email',
            visitors: 40,
            unique_conversions: 8,
            total_conversions: 9,
            conversion_rate: 20,
            revenue: { currency: 'USD', total: 400, average: 44.44 },
          },
        ],
      })
    },
  })
  const view = renderWithClient(<ConversionsCard {...range} />)

  const table = await view.findByRole('table', { name: 'Conversions' })
  expect(table.textContent).toContain('Purchase')
  expect(table.textContent).toContain('Event purchase')
  expect(table.textContent).toContain('4.0%')
  expect(table.textContent).toContain('$600.00')

  fireEvent.click(view.getByRole('button', { name: /^Purchase/ }))
  const list = await view.findByRole('list', { name: 'Purchase by Channel' })
  expect(list.textContent).toContain('Email')
  expect(list.textContent).toContain('20%')
  expect(breakdownQueries[0]?.get('dimension')).toBe('channel')
  api.restore()
})
