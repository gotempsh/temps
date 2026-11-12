// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SegmentVisitors } from './SegmentVisitors-components'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 7, slug: 'demo' } as ProjectResponse
const last = (values: string[]) => values[values.length - 1]

function visitor(id: number) {
  return {
    id,
    visitor_id: `visitor-${id}`,
    environment_id: 1,
    first_seen: '2026-01-01T00:00:00Z',
    last_seen: '2026-01-01T01:00:00Z',
    is_crawler: false,
  }
}

function renderSegment() {
  const offsets: string[] = []
  mockApi({
    'GET /analytics/visitors': (request) => {
      const offset = Number(new URL(request.url).searchParams.get('offset'))
      offsets.push(String(offset))
      return jsonResponse({
        visitors: [visitor(offset + 1)],
        total_count: 60,
        filtered_count: 60,
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const wrap = (ui: ReactNode) => (
    <QueryClientProvider client={client}>
      <MemoryRouter>{ui}</MemoryRouter>
    </QueryClientProvider>
  )
  const segment = (end: string, rangeScope: string) => (
    <SegmentVisitors
      project={project}
      dimension="browsers"
      value="Firefox"
      startDate={new Date(Date.parse(end) - 86_400_000)}
      endDate={new Date(end)}
      rangeScope={rangeScope}
      environment={undefined}
      onBack={() => {}}
    />
  )
  const view = render(wrap(segment('2026-01-02T00:00:00Z', '24h')))
  return {
    offsets,
    show: (end: string, rangeScope: string) =>
      view.rerender(wrap(segment(end, rangeScope))),
  }
}

test('Refresh moving the same range to now keeps the selected page', async () => {
  const { offsets, show } = renderSegment()
  await screen.findByText(/1–1 of 60/)
  fireEvent.click(screen.getByRole('button', { name: 'Next' }))
  await waitFor(() => expect(last(offsets)).toBe('25'))

  show('2026-01-02T00:05:00Z', '24h')

  await waitFor(() => expect(offsets).toHaveLength(3))
  expect(last(offsets)).toBe('25')
  expect(screen.getByText(/26–26 of 60/)).toBeTruthy()
})

test('picking another range goes back to the first page', async () => {
  const { offsets, show } = renderSegment()
  await screen.findByText(/1–1 of 60/)
  fireEvent.click(screen.getByRole('button', { name: 'Next' }))
  await waitFor(() => expect(last(offsets)).toBe('25'))

  show('2026-01-02T00:05:00Z', '7d')

  await waitFor(() => expect(last(offsets)).toBe('0'))
})
