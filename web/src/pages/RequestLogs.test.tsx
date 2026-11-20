// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  MemoryRouter,
  Route,
  Routes,
  useLocation,
  useNavigate,
} from 'react-router'
import type { ProjectResponse, ProxyLogResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { REQUEST_LOG_COLUMNS_STORAGE_KEY } from '@/components/logs/request-log-columns'
import RequestLogs from './RequestLogs'

installBrowserWindow()
afterEach(() => {
  cleanup()
  localStorage.removeItem(REQUEST_LOG_COLUMNS_STORAGE_KEY)
})

const project = { id: 42, slug: 'example', name: 'Example' } as ProjectResponse
const base = '/projects/example/request-logs'
const row: ProxyLogResponse = {
  id: 0,
  request_id: 'request/with spaces',
  timestamp: '2026-10-20T12:30:01Z',
  project_id: 42,
  host: 'app.example.test',
  path: '/api/items',
  method: 'GET',
  status_code: 200,
  response_time_ms: 0,
  routing_status: 'routed',
  request_source: 'proxy',
  is_system_request: false,
  shield_reasons: [],
}
const result = {
  logs: [row],
  page: 1,
  page_size: 25,
  total: 80,
  total_pages: 4,
}

function Navigation() {
  const location = useLocation()
  const navigate = useNavigate()
  return (
    <>
      <button onClick={() => navigate(-1)}>Browser back</button>
      <output data-testid="location">
        {location.pathname + location.search}
      </output>
    </>
  )
}

function mount({
  initial = base,
  read = () => jsonResponse(result),
  readDetail = () => jsonResponse(row),
}: {
  initial?: string
  read?: (request: Request) => Response | Promise<Response>
  readDetail?: () => Response
} = {}) {
  const requests: URL[] = []
  mockApi({
    'GET /projects/42/environments': () => jsonResponse([]),
    'GET /proxy-logs': (request) => {
      requests.push(new URL(request.url))
      return read(request)
    },
    'GET /proxy-logs/request/request%2Fwith%20spaces': readDetail,
  })
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity, gcTime: Infinity },
    },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[initial]}>
        <Navigation />
        <div data-console-scroll data-testid="scroller">
          <Routes>
            <Route
              path="/projects/example/request-logs/*"
              element={<RequestLogs project={project} />}
            />
          </Routes>
        </div>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { ...view, client, requests }
}

test.each(['Browser back', 'Back to logs'])(
  '%s restores filters, page, exact time window and both scroll positions',
  async (backLabel) => {
    const initial = `${base}?page=2&limit=50&method=GET&show_bots=no&sort_by=response_time_ms&sort_order=asc&time_range=7d&columns=path,timestamp,host&custom=keep`
    const view = mount({ initial })
    const link = await view.findByRole('link', {
      name: 'View GET /api/items request details',
    })
    expect(link.getAttribute('href')).toBe(
      `${base}/request%2Fwith%20spaces?ts=2026-10-20T12%3A30%3A01Z`
    )
    const scroller = view.getByTestId('scroller')
    const tableScroller = view.getByRole('table', {
      name: 'Request logs',
    }).parentElement!
    scroller.scrollTop = 640
    tableScroller.scrollLeft = 120

    await act(async () => {
      fireEvent.click(link)
    })
    expect(scroller.scrollTop).toBe(0)
    await view.findByRole('heading', { name: 'Request log' })
    await act(async () => {
      fireEvent.click(view.getByRole('button', { name: backLabel }))
    })
    await view.findByRole('table', { name: 'Request logs' })
    expect(view.getByTestId('location').textContent).toBe(initial)
    expect(scroller.scrollTop).toBe(640)
    expect(view.getByRole('table').parentElement!.scrollLeft).toBe(120)
    expect(view.requests).toHaveLength(1)
    expect(view.requests[0].searchParams.get('method')).toBe('GET')
    expect(view.requests[0].searchParams.get('page')).toBe('2')
    expect(view.getByRole('columnheader', { name: 'Host' })).toBeDefined()
    view.unmount()
    view.client.clear()
  }
)

test('restoration waits for rows when the cached list was evicted', async () => {
  let finish: ((response: Response) => void) | undefined
  let reads = 0
  const view = mount({
    read: () =>
      ++reads === 1
        ? jsonResponse(result)
        : new Promise((resolve) => {
            finish = resolve
          }),
  })
  const link = await view.findByRole('link', {
    name: 'View GET /api/items request details',
  })
  const originalWindow = view.requests[0].searchParams.get('end_date')
  view.getByTestId('scroller').scrollTop = 480
  await act(async () => {
    fireEvent.click(link)
  })
  view.client.removeQueries({
    predicate: (query) => {
      const key = query.queryKey[0]
      return (
        typeof key === 'object' &&
        key !== null &&
        '_id' in key &&
        key._id === 'getProxyLogs'
      )
    },
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Browser back' }))
  })
  await waitFor(() => expect(finish).toBeDefined())
  expect(view.getByTestId('scroller').scrollTop).toBe(0)
  await act(async () => {
    finish?.(jsonResponse(result))
  })
  await view.findByRole('link', { name: 'View GET /api/items request details' })
  expect(view.getByTestId('scroller').scrollTop).toBe(480)
  expect(view.requests[1].searchParams.get('end_date')).toBe(originalWindow)
  view.unmount()
  view.client.clear()
})

test('column picker persists choices, keeps the identity link and can reset', async () => {
  const view = mount()
  await view.findByRole('table')
  fireEvent.keyDown(view.getByRole('button', { name: 'Columns' }), {
    key: 'Enter',
  })
  const host = await view.findByRole('menuitemcheckbox', { name: 'Host' })
  expect(
    view
      .getByRole('menuitemcheckbox', { name: 'Path Required' })
      .getAttribute('aria-disabled')
  ).toBe('true')
  fireEvent.click(host)
  await waitFor(() => expect(host.getAttribute('aria-checked')).toBe('true'))
  fireEvent.keyDown(view.getByRole('menu'), { key: 'Escape' })
  await view.findByRole('columnheader', { name: 'Host' })
  expect(
    JSON.parse(localStorage.getItem(REQUEST_LOG_COLUMNS_STORAGE_KEY) ?? '[]')
  ).toContain('host')
  expect(view.getByTestId('location').textContent).toContain('columns=')
  fireEvent.keyDown(view.getByRole('button', { name: 'Columns' }), {
    key: 'Enter',
  })
  fireEvent.click(
    await view.findByRole('menuitem', { name: 'Reset to defaults' })
  )
  await waitFor(() =>
    expect(view.queryByRole('columnheader', { name: 'Host' })).toBeNull()
  )
  expect(
    view.getByRole('link', { name: 'View GET /api/items request details' })
  ).toBeDefined()
  view.unmount()
  view.client.clear()
})

test('sort controls reset pagination and expose the active direction', async () => {
  const view = mount({ initial: `${base}?page=2&method=POST&custom=keep` })
  await view.findByRole('table')
  fireEvent.click(
    view.getByRole('button', { name: 'Sort by Response time, descending' })
  )
  await waitFor(() =>
    expect(view.getByTestId('location').textContent).toContain(
      'sort_by=response_time_ms'
    )
  )
  const location = view.getByTestId('location').textContent ?? ''
  expect(location).not.toContain('page=2')
  expect(location).toContain('method=POST')
  expect(location).toContain('custom=keep')
  await waitFor(() =>
    expect(
      view
        .getByRole('columnheader', { name: /Response time/ })
        .getAttribute('aria-sort')
    ).toBe('descending')
  )
  expect(view.getByRole('table').textContent).toContain('0ms')
  expect(
    view.getByRole('link').closest('tr')?.getAttribute('onclick')
  ).toBeNull()
  view.unmount()
  view.client.clear()
})

test('failed requests offer retry instead of an empty state', async () => {
  let fail = true
  const view = mount({
    read: () =>
      fail ? jsonResponse({ title: 'Unavailable' }, 503) : jsonResponse(result),
  })
  await view.findByText('Could not load request logs')
  expect(view.queryByText('No request logs yet')).toBeNull()
  fail = false
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  await view.findByRole('link', { name: 'View GET /api/items request details' })
  view.unmount()
  view.client.clear()
})

test('background read failures retain usable rows', async () => {
  let fail = false
  const view = mount({
    read: () =>
      fail ? jsonResponse({ title: 'Unavailable' }, 503) : jsonResponse(result),
  })
  await view.findByRole('link', { name: 'View GET /api/items request details' })
  fail = true
  await act(async () => {
    await view.client.invalidateQueries()
  })
  await view.findByText('Request logs could not refresh')
  expect(
    view.getByRole('link', { name: 'View GET /api/items request details' })
  ).toBeDefined()
  view.unmount()
  view.client.clear()
})

test('a filtered empty list can clear all filters without dropping unrelated query state', async () => {
  const view = mount({
    initial: `${base}?method=POST&ai_agent=ExampleBot&path=/items&custom=keep`,
    read: () => jsonResponse({ ...result, logs: [], total: 0, total_pages: 0 }),
  })
  await view.findByText('No matching request logs')
  const clearButtons = view.getAllByRole('button', { name: 'Clear filters' })
  fireEvent.click(clearButtons[clearButtons.length - 1])
  await view.findByText('No request logs yet')
  expect(view.getByTestId('location').textContent).toContain('custom=keep')
  expect(view.getByTestId('location').textContent).not.toContain('ai_agent')
  view.unmount()
  view.client.clear()
})

test('invalid and out-of-range pagination is bounded', async () => {
  const view = mount({ initial: `${base}?page=999&limit=-5` })
  await view.findByRole('table')
  await waitFor(() => expect(view.requests).toHaveLength(2))
  expect(view.requests[0].searchParams.get('page_size')).toBe('25')
  expect(view.requests[1].searchParams.get('page')).toBe('4')
  view.unmount()
  view.client.clear()
})

test('a detail opened directly has a safe list destination', async () => {
  const view = mount({
    initial: `${base}/request%2Fwith%20spaces?ts=2026-10-20T12:30:01Z`,
  })
  await view.findByRole('heading', { name: 'Request log' })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Back to logs' }))
  })
  await view.findByRole('table')
  expect(view.getByTestId('location').textContent).toBe(base)
  view.unmount()
  view.client.clear()
})

test('a detail fetch failure still lets the user return to the saved list', async () => {
  const initial = `${base}?page=2&method=GET`
  const view = mount({
    initial,
    readDetail: () => jsonResponse({ title: 'Unavailable' }, 503),
  })
  const link = await view.findByRole('link', {
    name: 'View GET /api/items request details',
  })
  view.getByTestId('scroller').scrollTop = 320
  fireEvent.click(link)
  await view.findByText("Couldn't load log details")
  fireEvent.click(view.getByRole('button', { name: 'Back to logs' }))
  await view.findByRole('table')
  expect(view.getByTestId('location').textContent).toBe(initial)
  expect(view.getByTestId('scroller').scrollTop).toBe(320)
  view.unmount()
  view.client.clear()
})
