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
import { listTrafficFiltersQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { TrafficFilterResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { TrafficFiltersSection } from './TrafficFiltersSection'

installBrowserWindow()
afterEach(() => cleanup())

const FILTER: TrafficFilterResponse = {
  id: 9,
  project_id: 1,
  kind: 'referrer',
  value: 'spam-site.example',
  note: null,
  created_at: '2026-01-01T00:00:00Z',
}

function listOf(filters: TrafficFilterResponse[]) {
  return () =>
    jsonResponse({
      filters,
      max_filters_per_kind: 500,
      page: 1,
      page_size: 20,
      total: filters.length,
    })
}

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  })
}

function Section({
  client,
  projectId,
}: {
  client: QueryClient
  projectId: number
}) {
  return (
    <QueryClientProvider client={client}>
      <TrafficFiltersSection projectId={projectId} />
    </QueryClientProvider>
  )
}

function listQueries(client: QueryClient, projectId: number) {
  return client.getQueryCache().findAll({
    queryKey: listTrafficFiltersQueryKey({ path: { project_id: projectId } }),
  })
}

function listReads(api: ReturnType<typeof mockApi>, projectId: number) {
  return api.calls.filter(
    (call) =>
      call.method === 'GET' &&
      call.path === `/projects/${projectId}/analytics/traffic-filters`
  ).length
}

/**
 * Switch to project 2 while project 1's write is in flight, finish the write,
 * and check that project 1's list (not project 2's) was marked stale.
 */
async function finishAfterSwitching(
  view: ReturnType<typeof render>,
  client: QueryClient,
  api: ReturnType<typeof mockApi>,
  finish: () => void
) {
  view.rerender(<Section client={client} projectId={2} />)
  await waitFor(() => expect(listReads(api, 2)).toBe(1))
  await waitFor(() =>
    expect(listQueries(client, 2)[0]?.state.status).toBe('success')
  )

  await act(async () => {
    finish()
  })

  await waitFor(() =>
    expect(
      listQueries(client, 1).every((query) => query.state.isInvalidated)
    ).toBe(true)
  )
  expect(listQueries(client, 1).length).toBeGreaterThan(0)
  // Project 2's list was not touched by project 1's write.
  expect(listReads(api, 2)).toBe(1)
  expect(listQueries(client, 2)[0]?.state.isInvalidated).toBe(false)
}

test('a filter added before switching projects refreshes its own project', async () => {
  let finishCreate: (response: Response) => void = () => {}
  const api = mockApi({
    'GET /projects/1/analytics/traffic-filters': listOf([]),
    'GET /projects/2/analytics/traffic-filters': listOf([]),
    'POST /projects/1/analytics/traffic-filters': () =>
      new Promise<Response>((resolve) => {
        finishCreate = resolve
      }),
  })
  const client = createClient()
  const view = render(<Section client={client} projectId={1} />)
  await waitFor(() => expect(listReads(api, 1)).toBe(1))

  fireEvent.change(view.getByLabelText('Value'), {
    target: { value: 'spam-site.example' },
  })
  await act(async () => {
    fireEvent.click(view.getByLabelText('Exclude from analytics'))
  })
  await waitFor(() =>
    expect(api.called('POST', '/projects/1/analytics/traffic-filters')).toBe(
      true
    )
  )

  await finishAfterSwitching(view, client, api, () =>
    finishCreate(jsonResponse(FILTER, 201))
  )
  api.restore()
  client.clear()
})

test('a filter removed before switching projects refreshes its own project', async () => {
  let finishDelete: (response: Response) => void = () => {}
  const api = mockApi({
    'GET /projects/1/analytics/traffic-filters': listOf([FILTER]),
    'GET /projects/2/analytics/traffic-filters': listOf([]),
    'DELETE /projects/1/analytics/traffic-filters/9': () =>
      new Promise<Response>((resolve) => {
        finishDelete = resolve
      }),
  })
  const client = createClient()
  const view = render(<Section client={client} projectId={1} />)

  await act(async () => {
    fireEvent.click(
      await view.findByLabelText('Remove filter spam-site.example')
    )
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Remove' }))
  })
  await waitFor(() =>
    expect(
      api.called('DELETE', '/projects/1/analytics/traffic-filters/9')
    ).toBe(true)
  )

  await finishAfterSwitching(view, client, api, () =>
    finishDelete(new Response(null, { status: 204 }))
  )
  api.restore()
  client.clear()
})

test('a failed refresh after a delete keeps the rows and says they may be stale', async () => {
  let reads = 0
  const api = mockApi({
    'GET /projects/1/analytics/traffic-filters': () => {
      reads += 1
      return reads === 1
        ? listOf([FILTER])()
        : jsonResponse(
            { title: 'Service Unavailable', status: 503, detail: 'down' },
            503
          )
    },
    'DELETE /projects/1/analytics/traffic-filters/9': () =>
      new Response(null, { status: 204 }),
  })
  const client = createClient()
  const view = render(<Section client={client} projectId={1} />)

  await act(async () => {
    fireEvent.click(
      await view.findByLabelText('Remove filter spam-site.example')
    )
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Remove' }))
  })
  await waitFor(() => expect(reads).toBe(2))

  expect(await view.findByText('Traffic filters unavailable')).toBeTruthy()
  expect(view.getByText(/Showing last-known data/)).toBeTruthy()
  expect(view.getByRole('button', { name: 'Retry' })).toBeTruthy()
  // The last-known rows stay on screen rather than an error replacing them.
  expect(view.getByText('spam-site.example')).toBeTruthy()
  api.restore()
  client.clear()
})

test('switching projects starts the next one clean', async () => {
  const pages: string[] = []
  const recordPage =
    (projectId: number, total: number) => (request: Request) => {
      pages.push(
        `${projectId}:${new URL(request.url).searchParams.get('page')}`
      )
      return jsonResponse({
        filters: projectId === 1 ? [FILTER] : [],
        max_filters_per_kind: 500,
        page: 1,
        page_size: 20,
        total,
      })
    }
  const api = mockApi({
    'GET /projects/1/analytics/traffic-filters': recordPage(1, 45),
    'GET /projects/2/analytics/traffic-filters': recordPage(2, 0),
  })
  const client = createClient()
  const view = render(<Section client={client} projectId={1} />)

  // On project 1: a half-typed value, page 2, and a removal awaiting
  // confirmation.
  fireEvent.change(view.getByLabelText('Value'), {
    target: { value: 'half-typed.example' },
  })
  await act(async () => {
    fireEvent.click(await view.findByLabelText('Next page'))
  })
  await waitFor(() => expect(pages).toContain('1:2'))
  await act(async () => {
    fireEvent.click(
      await view.findByLabelText('Remove filter spam-site.example')
    )
  })
  expect(await view.findByRole('button', { name: 'Remove' })).toBeTruthy()

  view.rerender(<Section client={client} projectId={2} />)

  await waitFor(() => expect(pages).toContain('2:1'))
  // Project 2 never asked for project 1's page.
  expect(pages.filter((page) => page.startsWith('2:'))).toEqual(['2:1'])
  // The dialog about project 1's filter is gone, so it cannot be confirmed
  // against project 2.
  await waitFor(() =>
    expect(view.queryByRole('button', { name: 'Remove' })).toBeNull()
  )
  await waitFor(() =>
    expect((view.getByLabelText('Value') as HTMLInputElement).value).toBe('')
  )
  expect(api.calls.some((call) => call.method === 'DELETE')).toBe(false)
  api.restore()
  client.clear()
})
