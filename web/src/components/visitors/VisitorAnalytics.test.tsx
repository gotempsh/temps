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
import type { ProjectResponse, VisitorInfo } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { VISITOR_COLUMNS_STORAGE_KEY } from './visitor-columns'
import VisitorAnalytics from './VisitorAnalytics'

installBrowserWindow()
afterEach(() => {
  cleanup()
  localStorage.removeItem(VISITOR_COLUMNS_STORAGE_KEY)
})

const project = { id: 42, slug: 'example', name: 'Example' } as ProjectResponse
const base = '/projects/example/analytics/visitors'
const visitor: VisitorInfo = {
  id: 11,
  visitor_id: 'visitor-example-001',
  project_id: 42,
  environment_id: 3,
  first_seen: '2026-10-20T12:00:00Z',
  last_seen: '2026-10-21T12:00:00Z',
  is_crawler: false,
  country: 'Exampleland',
  city: 'Example City',
  user_agent:
    'Mozilla/5.0 (Macintosh; Intel Mac OS X) AppleWebKit/537.36 Chrome/120.0 Safari/537.36',
  ip_address: '192.0.2.1',
}
const result = { visitors: [visitor], filtered_count: 80, total_count: 80 }

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
}: {
  initial?: string
  read?: (request: Request) => Response | Promise<Response>
} = {}) {
  const requests: URL[] = []
  mockApi({
    'GET /analytics/visitors': (request) => {
      requests.push(new URL(request.url))
      return read(request)
    },
    'GET /analytics/visitor-facets': () =>
      jsonResponse({
        country: [{ value: 'Exampleland', count: 80 }],
        city: [],
        region: [],
        channel: [],
        referrer: [],
      }),
    'GET /projects/42/environments': () =>
      jsonResponse([{ id: 3, name: 'Production' }]),
    'GET /analytics/visitors/11': () => jsonResponse(visitor),
    'GET /analytics/visitors/11/sessions': () =>
      jsonResponse({ sessions: [], total_sessions: 0, visitor_id: 11 }),
    'GET /analytics/visitors/11/journey': () =>
      jsonResponse({
        sessions: [],
        total_sessions: 0,
        total_events: 0,
        visitor_id: 11,
      }),
    'GET /visitors/11/session-replays': () =>
      jsonResponse({ sessions: [], total: 0 }),
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
        <div data-console-scroll>
          <div data-console-scroll data-testid="scroller">
            <Routes>
              <Route
                path="/projects/example/analytics/visitors/*"
                element={<VisitorAnalytics project={project} />}
              />
            </Routes>
          </div>
        </div>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return {
    ...view,
    client,
    requests,
    dispose: () => {
      view.unmount()
      client.clear()
    },
  }
}

test.each(['Browser back', 'Back to visitors'])(
  '%s restores the visitor list URL, time window and scroll positions',
  async (back) => {
    const initial = `${base}?page=2&limit=50&visitor_type=humans&filter_country=Exampleland&activity=all&environment=3&time_range=7d&columns=visitor,location,ip_address&custom=keep`
    const view = mount({ initial })
    const link = await view.findByRole('link', {
      name: 'View Human visitor details',
    })
    expect(link.getAttribute('href')).toBe(`${base}/11`)
    expect(
      view.getByRole('table', { name: 'Visitors' }).outerHTML
    ).not.toContain(visitor.visitor_id)
    const scroller = view.getByTestId('scroller')
    scroller.scrollTop = 560
    view.getByRole('table', { name: 'Visitors' }).parentElement!.scrollLeft =
      100
    await act(async () => {
      fireEvent.click(link)
    })
    await view.findByRole('heading', { name: 'Visitor details' })
    expect(scroller.scrollTop).toBe(0)
    await act(async () => {
      fireEvent.click(view.getByRole('button', { name: back }))
    })
    await view.findByRole('table', { name: 'Visitors' })
    expect(view.getByTestId('location').textContent).toBe(initial)
    expect(scroller.scrollTop).toBe(560)
    expect(
      view.getByRole('table', { name: 'Visitors' }).parentElement!.scrollLeft
    ).toBe(100)
    expect(view.requests).toHaveLength(1)
    const params = view.requests[0].searchParams
    expect(params.get('include_crawlers')).toBe('false')
    expect(params.get('has_activity_only')).toBe('false')
    expect(params.get('filter_country')).toBe('Exampleland')
    expect(params.get('offset')).toBe('50')
    view.dispose()
  }
)

test('column selections persist and the visitor identity cannot be hidden', async () => {
  const view = mount()
  await view.findByRole('table', { name: 'Visitors' })
  fireEvent.keyDown(view.getByRole('button', { name: 'Columns' }), {
    key: 'Enter',
  })
  const ip = await view.findByRole('menuitemcheckbox', { name: 'IP address' })
  expect(
    view
      .getByRole('menuitemcheckbox', { name: 'Visitor Required' })
      .getAttribute('aria-disabled')
  ).toBe('true')
  fireEvent.click(ip)
  await waitFor(() => expect(ip.getAttribute('aria-checked')).toBe('true'))
  fireEvent.keyDown(view.getByRole('menu'), { key: 'Escape' })
  await view.findByRole('columnheader', { name: 'IP address' })
  expect(
    JSON.parse(localStorage.getItem(VISITOR_COLUMNS_STORAGE_KEY) ?? '[]')
  ).toContain('ip_address')
  expect(view.getByTestId('location').textContent).toContain('columns=')
  view.dispose()
  const revisit = mount()
  await revisit.findByRole('columnheader', { name: 'IP address' })
  revisit.dispose()
})

test('location filters do not open visitor details and reset the page', async () => {
  const view = mount({ initial: `${base}?page=2&custom=keep` })
  const country = await view.findByRole('button', {
    name: 'Filter by Exampleland',
  })
  fireEvent.click(country)
  await waitFor(() =>
    expect(view.getByTestId('location').textContent).toContain(
      'filter_country=Exampleland'
    )
  )
  expect(view.getByTestId('location').textContent).not.toContain('page=2')
  expect(view.getByTestId('location').textContent).toContain('custom=keep')
  expect(view.queryByRole('heading', { name: 'Visitor details' })).toBeNull()
  expect(
    view.getByRole('button', { name: 'Remove Country filter' })
  ).toBeDefined()
  fireEvent.click(view.getByRole('button', { name: 'Remove Country filter' }))
  await waitFor(() =>
    expect(view.getByTestId('location').textContent).not.toContain(
      'filter_country'
    )
  )
  view.dispose()
})

test('activity filtering is explicit and changing it resets pagination', async () => {
  const view = mount({ initial: `${base}?page=2&filters=open` })
  await view.findByRole('table', { name: 'Visitors' })
  expect(view.requests[0].searchParams.get('has_activity_only')).toBe('true')
  fireEvent.click(
    view.getByRole('switch', { name: 'Only visitors with activity' })
  )
  await waitFor(() =>
    expect(
      view.requests[view.requests.length - 1].searchParams.get(
        'has_activity_only'
      )
    ).toBe('false')
  )
  expect(view.getByTestId('location').textContent).toContain('activity=all')
  expect(view.getByTestId('location').textContent).not.toContain('page=2')
  view.dispose()
})

test('evicted visitor rows finish loading before scroll is restored', async () => {
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
    name: 'View Human visitor details',
  })
  const originalWindow = view.requests[0].searchParams.get('end_date')
  view.getByTestId('scroller').scrollTop = 400
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
        key._id === 'getVisitors'
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
  await view.findByRole('link', {
    name: 'View Human visitor details',
  })
  expect(view.getByTestId('scroller').scrollTop).toBe(400)
  expect(view.requests[1].searchParams.get('end_date')).toBe(originalWindow)
  view.dispose()
})

test('failed visitor reads offer retry and retain previously loaded rows on refresh failure', async () => {
  let fail = true
  const view = mount({
    read: () =>
      fail ? jsonResponse({ title: 'Unavailable' }, 503) : jsonResponse(result),
  })
  await view.findByText('Could not load visitors')
  expect(view.queryByText('No visitors yet')).toBeNull()
  fail = false
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  await view.findByRole('link', {
    name: 'View Human visitor details',
  })
  fail = true
  await act(async () => {
    await view.client.invalidateQueries()
  })
  await view.findByText('Visitors could not refresh')
  expect(
    view.getByRole('link', { name: 'View Human visitor details' })
  ).toBeDefined()
  view.dispose()
})

test('empty filtered results offer a complete reset without losing unrelated state', async () => {
  const view = mount({
    initial: `${base}?filter_country=Exampleland&activity=all&custom=keep`,
    read: () =>
      jsonResponse({
        ...result,
        visitors: [],
        filtered_count: 0,
        total_count: 0,
      }),
  })
  await view.findByText('No matching visitors')
  const buttons = view.getAllByRole('button', { name: 'Clear filters' })
  fireEvent.click(buttons[buttons.length - 1])
  await view.findByText('No visitors yet')
  expect(view.getByTestId('location').textContent).toBe(`${base}?custom=keep`)
  view.dispose()
})

test('out-of-range pagination is clamped and invalid page sizes are bounded', async () => {
  const view = mount({ initial: `${base}?page=999&limit=-4` })
  await view.findByRole('table', { name: 'Visitors' })
  await waitFor(() => expect(view.requests).toHaveLength(2))
  expect(view.requests[0].searchParams.get('limit')).toBe('25')
  expect(view.requests[1].searchParams.get('offset')).toBe('75')
  view.dispose()
})

test('direct visitor details have a safe destination back to the list', async () => {
  const view = mount({ initial: `${base}/11` })
  await view.findByRole('heading', { name: 'Visitor details' })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Back to visitors' }))
  })
  await view.findByRole('table', { name: 'Visitors' })
  expect(view.getByTestId('location').textContent).toBe(base)
  view.dispose()
})
