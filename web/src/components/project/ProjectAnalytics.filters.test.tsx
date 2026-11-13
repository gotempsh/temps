// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProjectResponse } from '@/api/client'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from '@/components/project-frame/project-environment-context'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'

// The globe pulls in three.js, which the test runner cannot load.
mock.module('@/components/analytics/EarthGlobe', () => ({
  EarthGlobe: () => null,
}))
const { AnalyticsFilters, ProjectAnalytics } =
  await import('./ProjectAnalytics')

installBrowserWindow()
afterEach(cleanup)

function filters() {
  return (
    <QueryClientProvider client={new QueryClient()}>
      <AnalyticsFilters
        project={{ id: 17 } as ProjectResponse}
        activeFilter="24hours"
        dateRange={undefined}
        selectedEnvironment={undefined}
        onFilterChange={() => {}}
        onDateRangeChange={() => {}}
        onEnvironmentChange={() => {}}
      />
    </QueryClientProvider>
  )
}

test('inside a project the analytics filters leave the environment to the bar', () => {
  const bar = {} as ProjectEnvironmentContextValue
  const view = render(
    <ProjectEnvironmentContext.Provider value={bar}>
      {filters()}
    </ProjectEnvironmentContext.Provider>
  )
  expect(view.queryByRole('combobox', { name: 'Environment' })).toBeNull()
})

test('on their own the analytics filters keep an environment picker', () => {
  const view = render(filters())
  expect(view.getByRole('combobox', { name: 'Environment' })).toBeTruthy()
})

test('without refresh keys the analytics filters offer no Refresh', () => {
  render(filters())
  expect(screen.queryByRole('button', { name: 'Refresh' })).toBeNull()
})

test('Refresh on the journey tab moves the window to now and refetches it', async () => {
  const requests: URLSearchParams[] = []
  mockApi({
    'GET /projects/17/has-events': () => jsonResponse({ has_events: true }),
    'GET /analytics/page-flow': (request) => {
      requests.push(new URL(request.url).searchParams)
      return jsonResponse({
        drop_off_points: [],
        top_entry_pages: [],
        top_exit_pages: [],
        transitions: [],
        total_pages: 3,
        total_sessions: 4000 + requests.length,
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/journey?filter=24hours']}>
        <ProjectAnalytics
          project={{ id: 17, slug: 'demo' } as ProjectResponse}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByText((4001).toLocaleString())
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await screen.findByText((4002).toLocaleString())
  await screen.findByText('Updated just now')
  expect(requests).toHaveLength(2)
  const [first, second] = requests
  expect(Date.parse(second.get('end_date')!)).toBeGreaterThan(
    Date.parse(first.get('end_date')!)
  )
  await waitFor(() =>
    expect(
      screen.getByRole('button', { name: 'Refresh' }).getAttribute('aria-busy')
    ).toBe('false')
  )
})

test('Refresh on the AI agents tab also reloads its timeline for a custom range', async () => {
  let timelineReads = 0
  mockApi({
    'GET /projects/17/has-events': () => jsonResponse({ has_events: true }),
    'GET /proxy-logs/stats/ai-agents/timeline': () => {
      timelineReads++
      return jsonResponse({ bucket: '1 hour', items: [] })
    },
    'GET /proxy-logs/stats/ai-agents': () => jsonResponse({ items: [] }),
    'GET /proxy-logs/stats/ai-pages': () => jsonResponse({ items: [] }),
    'GET /proxy-logs/stats/ai-status': () => jsonResponse({ items: [] }),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false, staleTime: Infinity } },
        })
      }
    >
      <MemoryRouter
        initialEntries={[
          '/ai-agents?filter=custom&from=2026-01-01T00:00:00.000Z&to=2026-01-02T00:00:00.000Z',
        ]}
      >
        <ProjectAnalytics
          project={{ id: 17, slug: 'demo' } as ProjectResponse}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await waitFor(() => expect(timelineReads).toBe(1))

  fireEvent.click(await screen.findByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(timelineReads).toBe(2))
})
