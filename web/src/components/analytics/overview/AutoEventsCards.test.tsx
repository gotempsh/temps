// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NotFoundCard, OutboundLinksCard } from './AutoEventsCards'

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

test('ranks outbound link destinations by clicks', async () => {
  let query: URLSearchParams | undefined
  const api = mockApi({
    'GET /analytics/event-properties': (request) => {
      query = new URL(request.url).searchParams
      return jsonResponse({
        event_name: 'outbound_click',
        property: 'url',
        total_count: 4,
        items: [
          {
            value: 'https://partner.example.org/pricing',
            count: 3,
            unique_visitors: 2,
            percentage: 75,
          },
          { value: null, count: 1, unique_visitors: 1, percentage: 25 },
        ],
      })
    },
  })
  const view = renderWithClient(<OutboundLinksCard {...range} />)

  const list = await view.findByRole('list', { name: 'Outbound links' })
  expect(list.textContent).toContain('partner.example.org/pricing')
  expect(list.textContent).toContain('(not set)')
  expect(list.textContent).toContain('75.0%')
  expect(query?.get('event_name')).toBe('outbound_click')
  expect(query?.get('property')).toBe('url')
  expect(query?.get('project_id')).toBe('7')
  api.restore()
})

test('an empty outbound card explains itself and links to the setup', async () => {
  const api = mockApi({
    'GET /analytics/event-properties': () =>
      jsonResponse({
        event_name: 'outbound_click',
        property: 'url',
        total_count: 0,
        items: [],
      }),
  })
  const view = renderWithClient(<OutboundLinksCard {...range} />)

  expect(await view.findByText(/No outbound link clicks/)).toBeTruthy()
  const setup = view.getByRole('link', { name: 'Tracker setup' })
  expect(setup.getAttribute('href')).toBe('/projects/site/analytics/setup')
  api.restore()
})

function pathsResponse() {
  return {
    dimensions: ['path'],
    metrics: ['requests', 'last_seen'],
    page: 1,
    page_size: 10,
    synthetic_excluded: true,
    total_groups: 1,
    total_pages: 1,
    rows: [
      {
        dimensions: [{ dimension: 'path', value: '/old-pricing' }],
        metrics: { requests: 40, last_seen: new Date().toISOString() },
      },
    ],
  }
}

test('lists 404 paths with the page that linked to them', async () => {
  const bodies: Array<{ dimensions: string[] }> = []
  const api = mockApi({
    'POST /projects/7/api-analytics/query': async (request) => {
      const body = (await request.json()) as { dimensions: string[] }
      bodies.push(body)
      if (!body.dimensions.includes('referrer')) {
        return jsonResponse(pathsResponse())
      }
      return jsonResponse({
        ...pathsResponse(),
        dimensions: ['path', 'referrer'],
        rows: [
          {
            dimensions: [
              { dimension: 'path', value: '/old-pricing' },
              { dimension: 'referrer', value: 'https://blog.example.org/post' },
            ],
            metrics: { requests: 30 },
          },
        ],
      })
    },
  })
  const view = renderWithClient(<NotFoundCard {...range} />)

  const list = await view.findByRole('list', {
    name: 'Paths answered with 404',
  })
  expect(list.textContent).toContain('/old-pricing')
  expect(list.textContent).toContain('40')
  expect(
    await view.findByText('Linked from blog.example.org/post')
  ).toBeTruthy()
  expect(bodies.map((body) => body.dimensions)).toEqual([
    ['path'],
    ['path', 'referrer'],
  ])
  api.restore()
})

test('still lists 404 paths when the server rejects the referrer dimension', async () => {
  const api = mockApi({
    'POST /projects/7/api-analytics/query': async (request) => {
      const body = (await request.json()) as { dimensions: string[] }
      return body.dimensions.includes('referrer')
        ? jsonResponse({ title: 'unknown variant `referrer`' }, 400)
        : jsonResponse(pathsResponse())
    },
  })
  const view = renderWithClient(<NotFoundCard {...range} />)

  const list = await view.findByRole('list', {
    name: 'Paths answered with 404',
  })
  expect(list.textContent).toContain('/old-pricing')
  expect(list.textContent).not.toContain('Linked from')
  api.restore()
})
