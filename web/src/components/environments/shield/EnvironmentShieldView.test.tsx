// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { useState } from 'react'
import { MemoryRouter } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import type { ShieldWindowValue } from '@/lib/shield'
import { EnvironmentShieldView } from './EnvironmentShieldView'
import { ShieldVerdictStats } from './ShieldSummary'

installBrowserWindow()
afterEach(cleanup)

const SHIELD = '/projects/3/environments/7/shield'

const SUMMARY = {
  project_id: 3,
  environment_id: 7,
  mode: 'log',
  mode_source: 'project',
  project_mode: 'log',
  window: '24h',
  start: '2026-10-29T10:00:00Z',
  end: '2026-10-30T10:00:00Z',
  storage: 'local',
  verdicts: { block: 4, challenge: 2, log: 597 },
  challenges: { issued: 0, solved: 0, failed: 0 },
  solve_times: { samples: 0, median_ms: null, p90_ms: null },
  sources: [{ source: 'synthetic-list', requests: 597 }],
  would_have: { blocked: 4, challenged: 2 },
}

function renderView(url = '/') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[url]}>
        <EnvironmentShieldView
          project={{ id: 3, slug: 'synthetic' } as ProjectResponse}
          environment={{ id: 7, name: 'staging' } as EnvironmentResponse}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the tab reads in three levels: mode, then Activity, then Tools', async () => {
  const api = mockApi({
    [`GET ${SHIELD}/summary`]: () => jsonResponse(SUMMARY),
    [`GET ${SHIELD}/events`]: () => jsonResponse({ events: [] }),
    [`GET ${SHIELD}/tokens`]: () => jsonResponse([]),
  })
  const view = renderView()

  const mode = await view.findByRole('heading', { level: 2, name: /^Mode: / })
  const groups = view
    .getAllByRole('heading', { level: 2 })
    .map((heading) => heading.textContent)
  expect(groups).toEqual([mode.textContent, 'Activity', 'Tools'])

  // Each section sits under its group, one level down.
  const sectionsOf = (group: string) => {
    const section = view
      .getByRole('heading', { level: 2, name: group })
      .closest('section')!
    // Empty states ("No Shield events yet") are headings too; skip them.
    return within(section)
      .getAllByRole('heading', { level: 3 })
      .map((heading) => heading.textContent)
      .filter((title) => !title?.startsWith('No '))
  }
  expect(sectionsOf('Activity')).toEqual([
    'Verdicts',
    'Challenges',
    'Recent events',
    'Requests by source',
  ])
  expect(sectionsOf('Tools')).toEqual([
    'Address lookup',
    'Bypass tokens',
    'Entries',
  ])

  // Logging only is a deliberate choice: what enforcing would have done is a
  // plain line, not an alert.
  expect(view.queryByRole('alert')).toBeNull()
  view.getByText(/Selective mode would have blocked/)
  expect(await view.findByText('synthetic-list')).toBeTruthy()
  api.restore()
})

test('a failed summary says the mode is unknown and offers a retry', async () => {
  let reads = 0
  const api = mockApi({
    [`GET ${SHIELD}/summary`]: () => {
      reads++
      return jsonResponse({ title: 'Unavailable', status: 503 }, 503)
    },
    [`GET ${SHIELD}/events`]: () => jsonResponse({ events: [] }),
    [`GET ${SHIELD}/tokens`]: () => jsonResponse([]),
  })
  const view = renderView()

  await view.findByRole('heading', { level: 2, name: 'Mode unknown' })
  expect(view.queryByRole('status', { name: 'Loading Shield mode' })).toBeNull()
  const before = reads
  fireEvent.click(view.getAllByRole('button', { name: 'Retry' })[0])
  await waitFor(() => expect(reads).toBeGreaterThan(before))
  api.restore()
})

test('every event, shown on its own, heads the page with an h2', async () => {
  const api = mockApi({
    [`GET ${SHIELD}/events`]: () => jsonResponse({ events: [] }),
  })
  const view = renderView('/?shield=events')

  expect(
    await view.findByRole('heading', { level: 2, name: 'All events' })
  ).toBeTruthy()
  api.restore()
})

test('while another window loads, the previous counts are not shown under it', async () => {
  const api = mockApi({
    [`GET ${SHIELD}/summary`]: (request) =>
      new URL(request.url).searchParams.get('window') === '1h'
        ? new Promise<Response>(() => {})
        : jsonResponse(SUMMARY),
  })
  function Harness() {
    const [timeWindow, setTimeWindow] = useState<ShieldWindowValue>('24h')
    return (
      <>
        <button type="button" onClick={() => setTimeWindow('1h')}>
          Last hour
        </button>
        <ShieldVerdictStats
          path={{ project_id: 3, environment_id: 7 }}
          timeWindow={timeWindow}
        />
      </>
    )
  }
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <Harness />
    </QueryClientProvider>
  )

  expect(await view.findByText('597')).toBeTruthy()
  fireEvent.click(view.getByRole('button', { name: 'Last hour' }))
  expect(
    await view.findByRole('status', { name: 'Loading Shield summary' })
  ).toBeTruthy()
  expect(view.queryByText('597')).toBeNull()
  api.restore()
})
