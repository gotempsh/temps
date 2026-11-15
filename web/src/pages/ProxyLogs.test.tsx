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
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import ProxyLogs from './ProxyLogs'

installBrowserWindow()
afterEach(cleanup)

function log(index: number) {
  return {
    id: index,
    host: 'app.example.test',
    path: `/checkout/${index}`,
    method: 'POST',
    status_code: 500,
    timestamp: '2026-11-01T10:00:00Z',
    request_id: `req-${index}`,
    request_source: 'proxy',
    routing_status: 'routed',
    is_system_request: false,
    shield_reasons: [],
  }
}

function renderAt(route: string) {
  const requests: URLSearchParams[] = []
  mockApi({
    'GET /proxy-logs': (request) => {
      requests.push(new URL(request.url).searchParams)
      // Traffic keeps arriving: each answer counts more requests.
      const total = 40 + requests.length
      return jsonResponse({
        logs: [log(requests.length)],
        page: 2,
        page_size: 20,
        total,
        total_pages: Math.ceil(total / 20),
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={[route]}>
          <ProxyLogs />
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
  return requests
}

test('Refresh keeps the filters and page and updates the total', async () => {
  const requests = renderAt(
    '/proxy-logs?method=POST&status_code=500&page=2&time_range=1h'
  )
  await screen.findByText('/checkout/1')
  expect(screen.getByText(/of 41/)).toBeTruthy()

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await screen.findByText('/checkout/2')
  expect(screen.getByText(/of 42/)).toBeTruthy()
  await screen.findByText('Updated just now')
  expect(requests).toHaveLength(2)
  const [first, second] = requests
  for (const key of ['method', 'status_code', 'page', 'page_size']) {
    expect(second.get(key)).toBe(first.get(key))
  }
  expect(second.get('method')).toBe('POST')
  expect(second.get('status_code')).toBe('500')
  expect(second.get('page')).toBe('2')
  // "Last hour" is resolved again, so the window moves forward with the clock.
  expect(Date.parse(second.get('start_date')!)).toBeGreaterThanOrEqual(
    Date.parse(first.get('start_date')!)
  )
})

test('the rows stay on screen while a refresh is in flight', async () => {
  let release: () => void = () => {}
  let calls = 0
  mockApi({
    'GET /proxy-logs': async () => {
      calls += 1
      if (calls > 1) await new Promise<void>((resolve) => (release = resolve))
      return jsonResponse({
        logs: [log(calls)],
        page: 1,
        page_size: 20,
        total: calls,
        total_pages: 1,
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/proxy-logs']}>
          <ProxyLogs />
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
  await screen.findByText('/checkout/1')

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(calls).toBe(2))
  expect(
    screen.getByRole('button', { name: 'Refresh' }).getAttribute('aria-busy')
  ).toBe('true')
  expect(screen.getByText('/checkout/1')).toBeTruthy()
  release()
  await screen.findByText('/checkout/2')
})

test('a refresh that leaves the page past the end goes to the last page, not an empty table', async () => {
  const pages: string[] = []
  mockApi({
    'GET /proxy-logs': (request) => {
      const page = new URL(request.url).searchParams.get('page')!
      pages.push(page)
      // Older requests aged out of the moved window: two pages left.
      const shrunk = pages.length > 1
      const total = shrunk ? 38 : 41
      return jsonResponse({
        logs: shrunk && page === '3' ? [] : [log(pages.length)],
        page: Number(page),
        page_size: 20,
        total,
        total_pages: Math.ceil(total / 20),
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/proxy-logs?page=3&time_range=1h']}>
          <ProxyLogs />
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
  await screen.findByText('/checkout/1')

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await screen.findByText('/checkout/3')
  expect(pages).toEqual(['3', '3', '2'])
  expect(screen.queryByText('No proxy logs found')).toBeNull()
})
