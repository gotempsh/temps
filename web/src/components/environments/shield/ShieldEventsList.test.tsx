// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter, useLocation } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ShieldEventsList } from './ShieldEventsList'
import { PREVIEW_SIZE, ShieldEventsPanel } from './ShieldEventsPanel'

installBrowserWindow()
afterEach(cleanup)

const EVENTS = 'GET /projects/3/environments/7/shield/events'
const PATH = { project_id: 3, environment_id: 7 }

function event(ip: string, extra: Record<string, unknown> = {}) {
  return {
    timestamp: '2026-10-27T10:00:00Z',
    kind: 'verdict',
    verdict: 'block',
    mode: 'selective',
    ip,
    host: 'app.example.com',
    path: '/wp-login.php',
    user_agent: 'curl/8.0',
    reasons: ['exploit probe'],
    request_id: `req-${ip}`,
    ...extra,
  }
}

/** Shows the address bar, so a test can read what the list put in it. */
function Location() {
  const location = useLocation()
  return <output aria-label="location">{location.search}</output>
}

function renderWith(children: ReactNode, path = '/?view=shield') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        {children}
        <Location />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const location = () =>
    view.getByRole('status', { name: 'location' }).textContent
  return { view, location }
}

/** Records each events request's query string. */
function recordEvents(
  answer: (query: Record<string, string>) => unknown = () => ({
    events: [event('203.0.113.7')],
    next_before: null,
  })
) {
  const queries: Record<string, string>[] = []
  const api = mockApi({
    [EVENTS]: (request) => {
      const query = Object.fromEntries(new URL(request.url).searchParams)
      queries.push(query)
      const body = answer(query)
      return body instanceof Response ? body : jsonResponse(body)
    },
  })
  return { api, queries, latest: () => queries[queries.length - 1] }
}

/** Picks an option in a Select, as a pointer would. */
async function choose(
  view: ReturnType<typeof render>,
  select: string,
  option: string
) {
  const trigger = view.getByRole('combobox', { name: select })
  await act(async () => {
    // Radix opens a Select on pointer-down only for a mouse.
    fireEvent.pointerDown(trigger, {
      button: 0,
      ctrlKey: false,
      pointerType: 'mouse',
    })
  })
  const item = await view.findByRole('option', { name: option })
  await act(async () => {
    fireEvent.click(item)
  })
}

test('the Shield tab shows only the newest events and links to all of them', async () => {
  const { api, queries } = recordEvents()
  let opened = 0
  const { view } = renderWith(
    <ShieldEventsPanel path={PATH} onShowAll={() => (opened += 1)} />
  )

  await view.findByText('203.0.113.7')
  expect(queries).toEqual([{ limit: String(PREVIEW_SIZE) }])
  expect(view.queryByRole('button', { name: /Older/ })).toBeNull()

  fireEvent.click(view.getByRole('button', { name: 'All events' }))
  expect(opened).toBe(1)
  api.restore()
})

test('filters reach the server and stay in the address bar', async () => {
  const { api, latest } = recordEvents()
  const { view, location } = renderWith(
    <ShieldEventsList path={PATH} onBack={() => {}} />,
    '/?view=shield&shield=events'
  )

  await view.findByText('203.0.113.7')
  expect(latest()).toEqual({ limit: '50' })

  await choose(view, 'Event kind', 'Challenge failed')
  await waitFor(() =>
    expect(latest()).toEqual({ limit: '50', kind: 'challenge_failed' })
  )
  await choose(view, 'Verdict', 'Challenge')
  fireEvent.change(view.getByLabelText('Client address'), {
    target: { value: '2001:db8::1' },
  })
  await waitFor(() =>
    expect(latest()).toEqual({
      limit: '50',
      kind: 'challenge_failed',
      verdict: 'challenge',
      ip: '2001:db8::1',
    })
  )
  const params = new URLSearchParams(location() ?? '')
  expect(params.get('view')).toBe('shield')
  expect(params.get('shield')).toBe('events')
  expect(params.get('kind')).toBe('challenge_failed')
  expect(params.get('verdict')).toBe('challenge')
  expect(params.get('ip')).toBe('2001:db8::1')

  fireEvent.click(view.getByRole('button', { name: 'Clear filters' }))
  await waitFor(() => expect(latest()).toEqual({ limit: '50' }))
  api.restore()
})

test('a shared link opens with its filters', async () => {
  const { api, queries } = recordEvents()
  const { view } = renderWith(
    <ShieldEventsList path={PATH} onBack={() => {}} />,
    '/?view=shield&shield=events&verdict=block&ip=198.51.100.23'
  )

  await view.findByText('203.0.113.7')
  expect(queries[0]).toEqual({
    limit: '50',
    verdict: 'block',
    ip: '198.51.100.23',
  })
  api.restore()
})

test('older pages follow the cursor, and a new filter starts from the newest', async () => {
  const { api, latest } = recordEvents((query) =>
    query.before
      ? { events: [event('198.51.100.23')], next_before: null }
      : { events: [event('203.0.113.7')], next_before: 'cursor-1' }
  )
  const { view } = renderWith(
    <ShieldEventsList path={PATH} onBack={() => {}} />,
    '/?view=shield&shield=events'
  )

  await view.findByText('203.0.113.7')
  fireEvent.click(view.getByRole('button', { name: /Older/ }))
  await view.findByText('198.51.100.23')
  expect(latest()).toEqual({ limit: '50', before: 'cursor-1' })

  await choose(view, 'Verdict', 'Log')
  await waitFor(() => expect(latest()).toEqual({ limit: '50', verdict: 'log' }))
  await view.findByText('203.0.113.7')
  api.restore()
})

test('an address the server refuses shows why', async () => {
  const { api } = recordEvents((query) =>
    query.ip
      ? jsonResponse(
          {
            title: 'Invalid request',
            detail: 'ip must be an IP address',
            status: 400,
          },
          400
        )
      : { events: [], next_before: null }
  )
  const { view } = renderWith(
    <ShieldEventsList path={PATH} onBack={() => {}} />,
    '/?view=shield&shield=events&ip=not-an-address'
  )

  await view.findByText(/ip must be an IP address/)
  api.restore()
})

test('the way back leaves the full list', async () => {
  const { api } = recordEvents()
  let back = 0
  const { view } = renderWith(
    <ShieldEventsList path={PATH} onBack={() => (back += 1)} />,
    '/?view=shield&shield=events'
  )

  await view.findByText('203.0.113.7')
  fireEvent.click(view.getByRole('button', { name: 'Shield overview' }))
  expect(back).toBe(1)
  api.restore()
})
