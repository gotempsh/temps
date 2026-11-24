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
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { AiCrawlerActivityFeed } from './AiCrawlerActivityFeed'

installBrowserWindow()
afterEach(cleanup)

function crawl(index: number) {
  return {
    id: index,
    project_id: 17,
    host: 'docs.example.test',
    path: `/guides/${index}`,
    method: 'GET',
    status_code: 200,
    timestamp: '2026-11-01T10:00:00Z',
    request_id: `req-${index}`,
    request_source: 'proxy',
    routing_status: 'routed',
    is_system_request: false,
    is_bot: true,
    bot_name: 'GPTBot',
    shield_reasons: [],
  }
}

test('a refresh that leaves the page past the end goes to the last page, not "No AI crawler activity"', async () => {
  const pages: string[] = []
  mockApi({
    'GET /proxy-logs': (request) => {
      const page = new URL(request.url).searchParams.get('page')!
      pages.push(page)
      // The oldest crawls aged out of the moved window: two pages left.
      const shrunk = pages.length > 1
      const total = shrunk ? 49 : 51
      return jsonResponse({
        logs: shrunk && page === '3' ? [] : [crawl(pages.length)],
        page: Number(page),
        page_size: 25,
        total,
        total_pages: Math.ceil(total / 25),
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/ai-crawlers?range=24h&page=3']}>
        <AiCrawlerActivityFeed projectId={17} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const shows = (text: string) =>
    waitFor(() => expect(document.body.textContent).toContain(text))
  await shows('docs.example.test/guides/1')

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await shows('docs.example.test/guides/3')
  expect(pages).toEqual(['3', '3', '2'])
  expect(screen.queryByText(/No AI crawler activity/)).toBeNull()
})
