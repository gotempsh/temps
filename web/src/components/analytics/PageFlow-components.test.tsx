// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, within } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { PageFlow } from './PageFlow-components'

installBrowserWindow()
afterEach(cleanup)

const page = (
  path: string,
  entries: number,
  views: number,
  bounce: number
) => ({
  page_path: path,
  entry_count: entries,
  exit_count: entries,
  total_views: views,
  bounce_count: 0,
  bounce_rate: bounce,
  entry_rate: 0.1,
  exit_rate: bounce,
  avg_time_on_page: views,
})

// Same shape as the report: views 15, 12, 1, 1, 1 and one low-entry page
// (/f) with 12 views that only a views sort moves up.
const PAGES = [
  page('/a', 15, 15, 0.2),
  page('/b', 12, 12, 0.5),
  page('/c', 1, 1, 0.9),
  page('/d', 1, 1, 0.1),
  page('/e', 1, 1, 0.3),
  page('/f', 1, 12, 0.7),
]

function columnValues(table: HTMLElement, header: string): string[] {
  const headers = within(table).getAllByRole('columnheader')
  const index = headers.findIndex((th) => th.textContent?.startsWith(header))
  return Array.from(table.querySelectorAll('tbody tr')).map(
    (row) => row.children[index]?.textContent?.trim() ?? ''
  )
}

test('clicking a Journey column header reorders the rows, and clicking again reverses them', async () => {
  const api = mockApi({
    'GET /analytics/page-flow': () =>
      jsonResponse({
        top_entry_pages: PAGES,
        top_exit_pages: [],
        drop_off_points: [],
        transitions: [],
        total_pages: PAGES.length,
        total_sessions: 31,
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <PageFlow
        project={{ id: 1, slug: 'demo', name: 'demo' } as ProjectResponse}
        startDate={new Date('2026-10-11T00:00:00Z')}
        endDate={new Date('2026-10-12T00:00:00Z')}
        environment={undefined}
      />
    </QueryClientProvider>
  )
  const table = (await view.findAllByRole('table'))[0]!
  expect(columnValues(table, 'Views')).toEqual([
    '15',
    '12',
    '1',
    '1',
    '1',
    '12',
  ])

  const sortByViews = within(table).getByRole('button', {
    name: /Sort by Views/,
  })
  fireEvent.click(sortByViews)
  expect(columnValues(table, 'Views')).toEqual([
    '15',
    '12',
    '12',
    '1',
    '1',
    '1',
  ])
  expect(table.querySelector('th[aria-sort]')?.getAttribute('aria-sort')).toBe(
    'descending'
  )

  fireEvent.click(sortByViews)
  expect(columnValues(table, 'Views')).toEqual([
    '1',
    '1',
    '1',
    '12',
    '12',
    '15',
  ])

  fireEvent.click(within(table).getByRole('button', { name: /Sort by Page/ }))
  expect(columnValues(table, 'Page')).toEqual([
    '/f',
    '/e',
    '/d',
    '/c',
    '/b',
    '/a',
  ])

  fireEvent.click(
    within(table).getByRole('button', { name: /Sort by Bounce rate/ })
  )
  expect(columnValues(table, 'Bounce rate')[0]).toBe('90.0%')
  api.restore()
})
