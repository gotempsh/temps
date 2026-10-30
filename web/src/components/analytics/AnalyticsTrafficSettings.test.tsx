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
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  getFilteredTrafficQueryKey,
  getProjectAnalyticsSettingsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ProjectAnalyticsSettingsResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { AnalyticsTrafficSettings } from './AnalyticsTrafficSettings'

installBrowserWindow()
afterEach(() => cleanup())

function settings(
  projectId: number,
  excludeDatacenter = true
): ProjectAnalyticsSettingsResponse {
  return {
    project_id: projectId,
    exclude_datacenter_traffic: excludeDatacenter,
    exclude_spam_referrers: true,
    spam_referrer_list_size: 2300,
    updated_at: null,
    datacenter_detection: {
      available: true,
      asn_database_built_at: null,
      reason: null,
      setup_path: null,
    },
  }
}

function project(id: number): ProjectResponse {
  return { id, name: `Project ${id}`, slug: `project-${id}` } as ProjectResponse
}

function settingsKey(projectId: number) {
  return getProjectAnalyticsSettingsQueryKey({
    path: { project_id: projectId },
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

function Page({
  client,
  projectId,
}: {
  client: QueryClient
  projectId: number
}) {
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AnalyticsTrafficSettings project={project(projectId)} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** Every read the page makes, for both projects. */
function pageReads(projectId: number) {
  return {
    [`GET /projects/${projectId}/analytics/settings`]: () =>
      jsonResponse(settings(projectId)),
    [`GET /projects/${projectId}/analytics/filtered-traffic`]: () =>
      jsonResponse({
        project_id: projectId,
        days: 7,
        counted_events: 0,
        filtered_events: 0,
        reasons: [],
        since: '2026-01-01T00:00:00Z',
      }),
    [`GET /projects/${projectId}/analytics/traffic-filters`]: () =>
      jsonResponse({
        filters: [],
        max_filters_per_kind: 500,
        page: 1,
        page_size: 20,
        total: 0,
      }),
  }
}

test('a save that finishes after switching projects updates only its own project', async () => {
  let finishSave: (response: Response) => void = () => {}
  const api = mockApi({
    ...pageReads(1),
    ...pageReads(2),
    'PATCH /projects/1/analytics/settings': () =>
      new Promise<Response>((resolve) => {
        finishSave = resolve
      }),
  })
  const client = createClient()
  const view = render(<Page client={client} projectId={1} />)

  const toggle = await view.findByRole('switch', {
    name: 'Exclude datacenter traffic',
  })
  await act(async () => {
    fireEvent.click(toggle)
  })
  await waitFor(() =>
    expect(api.called('PATCH', '/projects/1/analytics/settings')).toBe(true)
  )

  // The user moves to another project while the save is still in flight.
  view.rerender(<Page client={client} projectId={2} />)
  await waitFor(() => expect(client.getQueryData(settingsKey(2))).toBeDefined())

  await act(async () => {
    finishSave(jsonResponse(settings(1, false)))
  })

  await waitFor(() =>
    expect(
      client.getQueryData<ProjectAnalyticsSettingsResponse>(settingsKey(1))
        ?.exclude_datacenter_traffic
    ).toBe(false)
  )
  expect(client.getQueryData(settingsKey(2))).toEqual(settings(2))
  api.restore()
  client.clear()
})

test('a refresh that read before a save cannot undo it', async () => {
  let reads = 0
  let finishStaleRead: (response: Response) => void = () => {}
  const api = mockApi({
    ...pageReads(1),
    'GET /projects/1/analytics/settings': () => {
      reads += 1
      if (reads === 1) return jsonResponse(settings(1))
      // A background refresh that read the settings before the save.
      return new Promise<Response>((resolve) => {
        finishStaleRead = resolve
      })
    },
    'PATCH /projects/1/analytics/settings': () =>
      jsonResponse(settings(1, false)),
  })
  const client = createClient()
  const view = render(<Page client={client} projectId={1} />)
  const toggle = await view.findByRole('switch', {
    name: 'Exclude datacenter traffic',
  })

  void client.refetchQueries({ queryKey: settingsKey(1) })
  await waitFor(() => expect(reads).toBe(2))
  await act(async () => {
    fireEvent.click(toggle)
  })
  await waitFor(() =>
    expect(
      client.getQueryData<ProjectAnalyticsSettingsResponse>(settingsKey(1))
        ?.exclude_datacenter_traffic
    ).toBe(false)
  )

  // The stale read comes back after the save succeeded.
  await act(async () => {
    finishStaleRead(jsonResponse(settings(1, true)))
  })
  await new Promise((resolve) => setTimeout(resolve, 20))

  expect(
    client.getQueryData<ProjectAnalyticsSettingsResponse>(settingsKey(1))
      ?.exclude_datacenter_traffic
  ).toBe(false)
  expect(
    view
      .getByRole('switch', { name: 'Exclude datacenter traffic' })
      .getAttribute('aria-checked')
  ).toBe('false')
  api.restore()
  client.clear()
})

function summaryKey(projectId: number) {
  return getFilteredTrafficQueryKey({
    path: { project_id: projectId },
    query: { days: 7 },
  })
}

function failRefresh(
  client: QueryClient,
  projectId: number,
  queryKey: readonly unknown[] = settingsKey(projectId)
) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({
      status: 'error',
      error: { title: 'Service Unavailable', status: 503 } as unknown as Error,
      fetchStatus: 'idle',
    })
}

test('a failed background refresh keeps the loaded switches on screen', () => {
  const client = createClient()
  client.setQueryData(settingsKey(1), settings(1))
  failRefresh(client, 1)

  const html = renderToStaticMarkup(<Page client={client} projectId={1} />)

  expect(html).toContain('Exclude datacenter traffic')
  expect(html).toContain('Exclude known referrer spam')
  // Speed re-checks hosting-provider IPs when read, so the note must not
  // promise that older Speed samples keep their classification.
  expect(html).toContain('the setting applies to older samples too')
  // Server-side tracking is the case users need to know is unaffected.
  expect(html).toContain('authenticated')
  expect(html).not.toContain('Could not load traffic filters')
  // ...and says they may be stale, with a way to retry.
  expect(html).toContain('Traffic filter settings unavailable')
  expect(html).toContain('Showing last-known data')
  client.clear()
})

test('a failed summary refresh keeps the counts and says they are stale', () => {
  const client = createClient()
  client.setQueryData(settingsKey(1), settings(1))
  client.setQueryData(summaryKey(1), {
    project_id: 1,
    days: 7,
    counted_events: 900,
    filtered_events: 100,
    reasons: [],
    since: '2026-01-01T00:00:00Z',
  })
  failRefresh(client, 1, summaryKey(1))

  const html = renderToStaticMarkup(<Page client={client} projectId={1} />)

  expect(html).toContain('Filtered-traffic summary unavailable')
  expect(html).toContain('Showing last-known data')
  expect(html).toContain('10%')
  expect(html).not.toContain('Could not load the filtered-traffic summary')
  client.clear()
})

test('a failed first load explains the failure instead of the switches', () => {
  const client = createClient()
  failRefresh(client, 1)

  const html = renderToStaticMarkup(<Page client={client} projectId={1} />)

  expect(html).toContain('Could not load traffic filters')
  expect(html).not.toContain('Exclude datacenter traffic')
  client.clear()
})
