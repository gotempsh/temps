// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  MutationCache,
  QueryClient,
  QueryClientProvider,
} from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MemoryRouter, useLocation } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { ShieldProposalsPage } from './ShieldProposalsPage'

installBrowserWindow()
afterEach(cleanup)

const LIST = 'GET /settings/shield/proposals'

const PROPOSAL = {
  id: 41,
  kind: 'ip',
  subject: '203.0.113.7',
  verdict: 'block',
  reason: 'exploit CVE-2021-41773',
  hits: 3,
  first_seen: '2026-10-27T09:00:00Z',
  last_seen: '2026-10-27T10:00:00Z',
  expires_at: '2026-11-03T10:00:00Z',
  status: 'pending',
  decided_at: null,
  decided_by: null,
}

const FINGERPRINT = {
  ...PROPOSAL,
  id: 42,
  kind: 'ja4',
  subject: 't13d1516h2_8daaf6152771_02713d6af862',
  verdict: 'challenge',
  reason: 'exploits from 20 addresses',
  hits: 20,
}

/** A page as the server sends it. */
function page(
  items: unknown[],
  { total = items.length, page = 1, page_size = 25 } = {}
) {
  return jsonResponse({ items, total, page, page_size })
}

/** The API with the Shield settings the page also reads. */
function mockPage(
  routes: Parameters<typeof mockApi>[0],
  { learning = true } = {}
) {
  return mockApi({
    'GET /settings/shield': () =>
      jsonResponse({ share_observations: learning }),
    ...routes,
  })
}

/** Shows the address bar, so a test can read what the page put in it. */
function Location() {
  const location = useLocation()
  return <output aria-label="location">{location.search}</output>
}

function renderPage(path = '/settings/shield-proposals') {
  const failures: string[] = []
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    // What App.tsx's handler announces: the authored title.
    mutationCache: new MutationCache({
      onError: (_error, _variables, _context, mutation) => {
        failures.push(String(mutation.meta?.errorTitle))
      },
    }),
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <BreadcrumbProvider>
          <ShieldProposalsPage />
          <Location />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const location = () => view.getByRole('status', { name: 'location' })
  return { view, failures, location }
}

/** Picks a state in the filter, as a pointer would. */
async function chooseFilter(
  view: ReturnType<typeof render>,
  label: 'Pending' | 'Approved' | 'Rejected' | 'All'
) {
  const trigger = view.getByRole('combobox', { name: 'Show proposals' })
  await act(async () => {
    // Radix opens a Select on pointer-down only for a mouse.
    fireEvent.pointerDown(trigger, {
      button: 0,
      ctrlKey: false,
      pointerType: 'mouse',
    })
  })
  const option = await view.findByRole('option', { name: label })
  await act(async () => {
    fireEvent.click(option)
  })
}

/** Moves to the next page with the pager's own button. */
async function nextPage(view: ReturnType<typeof render>) {
  await act(async () => {
    fireEvent.click(view.getAllByRole('button', { name: 'Next page' })[0])
  })
}

/** The query string of the newest list request. */
function latest(requests: URL[]): Record<string, string> | undefined {
  const queries = listQueries(requests)
  return queries[queries.length - 1]
}

/** Requests the list was sent, with their query strings. */
function listQueries(requests: URL[]) {
  return requests.map((url) => Object.fromEntries(url.searchParams))
}

test('a pending proposal is approved and leaves the pending list', async () => {
  let status = 'pending'
  const requests: URL[] = []
  const api = mockPage({
    [LIST]: (request) => {
      const url = new URL(request.url)
      requests.push(url)
      const wanted = url.searchParams.get('status')
      return page(
        [{ ...PROPOSAL, status }].filter(
          (row) => !wanted || row.status === wanted
        )
      )
    },
    'POST /settings/shield/proposals/41/approve': () => {
      status = 'approved'
      return jsonResponse({ ...PROPOSAL, status })
    },
  })
  const { view } = renderPage()

  await view.findByText('203.0.113.7')
  expect(listQueries(requests)[0]).toEqual({
    status: 'pending',
    page: '1',
    page_size: '25',
  })
  view.getByText('exploit CVE-2021-41773')
  fireEvent.click(view.getByRole('button', { name: 'Approve 203.0.113.7' }))

  await view.findByText('No proposals to review')
  expect(api.called('POST', '/settings/shield/proposals/41/approve')).toBe(true)
  expect(api.called('POST', '/settings/shield/proposals/41/reject')).toBe(false)
  api.restore()
})

test('an approved proposal can be rejected, and offers no second approval', async () => {
  const api = mockPage({
    // The server answers whatever the filter; the row's state drives the buttons.
    [LIST]: () => page([{ ...PROPOSAL, status: 'approved' }]),
    'POST /settings/shield/proposals/41/reject': () =>
      jsonResponse({ ...PROPOSAL, status: 'rejected' }),
  })
  const { view } = renderPage()

  await view.findByText('approved')
  expect(view.queryByRole('button', { name: 'Approve 203.0.113.7' })).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Reject 203.0.113.7' }))
  await waitFor(() =>
    expect(api.called('POST', '/settings/shield/proposals/41/reject')).toBe(
      true
    )
  )
  api.restore()
})

test('with learning off, the page says approvals do not apply', async () => {
  const api = mockPage({ [LIST]: () => page([PROPOSAL]) }, { learning: false })
  const { view } = renderPage()

  await view.findByText('203.0.113.7')
  await view.findByText('Learning is off')
  view.getByText(/Approved proposals do not apply/)
  expect(
    view
      .getByRole('link', { name: 'Learn from attacks across all services' })
      .getAttribute('href')
  ).toBe('/settings/security#shield')
  api.restore()
})

test('with learning on, there is no warning', async () => {
  const api = mockPage({ [LIST]: () => page([PROPOSAL]) })
  const { view } = renderPage()

  await view.findByText('203.0.113.7')
  expect(view.queryByText('Learning is off')).toBeNull()
  api.restore()
})

test('the filter asks the server for that state, from the first page', async () => {
  const requests: URL[] = []
  const api = mockPage({
    [LIST]: (request) => {
      const url = new URL(request.url)
      requests.push(url)
      const wanted = url.searchParams.get('status')
      return wanted === 'approved'
        ? page([{ ...PROPOSAL, status: 'approved' }])
        : page([PROPOSAL])
    },
  })
  const { view, location } = renderPage()
  await view.findByText('203.0.113.7')

  await chooseFilter(view, 'Approved')
  await waitFor(() => expect(location().textContent).toBe('?status=approved'))

  await view.findByText('approved')
  expect(latest(requests)).toEqual({
    status: 'approved',
    page: '1',
    page_size: '25',
  })

  await chooseFilter(view, 'All')
  await waitFor(() => expect(latest(requests)?.status).toBeUndefined())
  api.restore()
})

test('a failed approval keeps the row, says why, and lets you try again', async () => {
  let listed = 0
  const api = mockPage({
    [LIST]: () => {
      listed++
      return page([PROPOSAL])
    },
    'POST /settings/shield/proposals/41/approve': () =>
      jsonResponse(
        { title: 'Internal Server Error', detail: 'database unavailable' },
        500
      ),
  })
  const { view, failures } = renderPage()
  await view.findByText('203.0.113.7')
  const before = listed

  const approve = view.getByRole('button', { name: 'Approve 203.0.113.7' })
  fireEvent.click(approve)
  await waitFor(() =>
    expect(failures).toEqual(['Failed to approve the proposal'])
  )
  // Nothing changed, so the list is not read again, and the row is still
  // there to decide.
  expect(listed).toBe(before)
  view.getByText('203.0.113.7')
  await waitFor(() => expect(approve.hasAttribute('disabled')).toBe(false))
  api.restore()
})

test('an address stays on one line and a fingerprint may wrap', async () => {
  const api = mockPage({ [LIST]: () => page([PROPOSAL, FINGERPRINT]) })
  const { view } = renderPage()

  const address = await view.findByText('203.0.113.7')
  expect(address.className).toContain('whitespace-nowrap')
  expect(address.className).not.toContain('break-all')
  view.getByText('Address')

  const fingerprint = view.getByText(FINGERPRINT.subject)
  expect(fingerprint.className).toContain('break-all')
  view.getByText('JA4 fingerprint')
  view.getByRole('button', { name: `Approve ${FINGERPRINT.subject}` })
  api.restore()
})

test('pages and the search go to the server, and a search starts at page 1', async () => {
  const requests: URL[] = []
  // Page n holds 198.51.10n.1 to .25.
  const rows = (at: number) =>
    Array.from({ length: 25 }, (_, i) => ({
      ...PROPOSAL,
      id: at * 100 + i,
      subject: `198.51.${100 + at}.${i + 1}`,
    }))
  const api = mockPage({
    [LIST]: (request) => {
      const url = new URL(request.url)
      requests.push(url)
      const at = Number(url.searchParams.get('page'))
      if (url.searchParams.get('q')) {
        return page([{ ...PROPOSAL, subject: '198.51.100.120' }])
      }
      return page(rows(at), { total: 130, page: at })
    },
  })
  const { view } = renderPage()
  await view.findByText('198.51.101.1')
  expect(view.getAllByText(/of 130/).length).toBeGreaterThan(0)

  await nextPage(view)
  await view.findByText('198.51.102.1')
  expect(view.queryByText('198.51.101.1')).toBeNull()
  expect(latest(requests)?.page).toBe('2')

  fireEvent.change(view.getByRole('searchbox', { name: 'Search proposals' }), {
    target: { value: ' 100.12 ' },
  })
  await view.findByText('198.51.100.120')
  // The first request with the search already asks for page 1.
  expect(listQueries(requests).find((query) => query.q)).toEqual({
    status: 'pending',
    q: '100.12',
    page: '1',
    page_size: '25',
  })
  api.restore()
})

test('a search that matches nothing says so', async () => {
  const api = mockPage({
    [LIST]: (request) =>
      new URL(request.url).searchParams.get('q') ? page([]) : page([PROPOSAL]),
  })
  const { view } = renderPage()
  await view.findByText('203.0.113.7')

  fireEvent.change(view.getByRole('searchbox', { name: 'Search proposals' }), {
    target: { value: '192.0.2.1' },
  })
  await view.findByText('No proposals match')
  api.restore()
})

test('deciding the only row on the last page goes back a page', async () => {
  let pending = 26
  const requests: URL[] = []
  const api = mockPage({
    [LIST]: (request) => {
      const url = new URL(request.url)
      requests.push(url)
      const at = Number(url.searchParams.get('page'))
      const first = (at - 1) * 25
      const items = Array.from(
        { length: Math.max(0, Math.min(25, pending - first)) },
        (_, i) => ({
          ...PROPOSAL,
          id: first + i + 1,
          subject: `198.51.100.${first + i + 1}`,
        })
      )
      return page(items, { total: pending, page: at })
    },
    'POST /settings/shield/proposals/26/approve': () => {
      pending = 25
      return jsonResponse({ ...PROPOSAL, id: 26, status: 'approved' })
    },
  })
  const { view } = renderPage()
  await view.findByText('198.51.100.1')

  await nextPage(view)
  await view.findByText('198.51.100.26')
  fireEvent.click(view.getByRole('button', { name: 'Approve 198.51.100.26' }))

  await view.findByText('198.51.100.1')
  expect(view.queryByText('No proposals to review')).toBeNull()
  expect(latest(requests)?.page).toBe('1')
  api.restore()
})

test('a shared link opens on its filter, search and page', async () => {
  const requests: URL[] = []
  const api = mockPage({
    [LIST]: (request) => {
      const url = new URL(request.url)
      requests.push(url)
      return page([{ ...PROPOSAL, status: 'rejected' }], {
        total: 30,
        page: 2,
        page_size: 10,
      })
    },
  })
  const { view } = renderPage(
    '/settings/shield-proposals?status=rejected&q=203.0&page=2&per_page=10'
  )

  await view.findByText('203.0.113.7')
  expect(listQueries(requests)[0]).toEqual({
    status: 'rejected',
    q: '203.0',
    page: '2',
    page_size: '10',
  })
  expect(
    (
      view.getByRole('searchbox', {
        name: 'Search proposals',
      }) as HTMLInputElement
    ).value
  ).toBe('203.0')
  expect(
    view.getByRole('combobox', { name: 'Show proposals' }).textContent
  ).toContain('Rejected')
  api.restore()
})
