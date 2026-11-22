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
import type {
  ActivityStatus,
  EnvironmentResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ActivityReportPage } from './ActivityReport'

installBrowserWindow()
afterEach(cleanup)

const project = {
  id: 7,
  slug: 'sample-site',
  name: 'Sample site',
} as ProjectResponse

const environment = {
  id: 3,
  name: 'production',
  main_url: 'https://app.example.com',
  is_preview: false,
} as EnvironmentResponse

function activityStatus(
  overrides: Partial<ActivityStatus> = {}
): ActivityStatus {
  return {
    has_recent_activity: true,
    selected_environment_id: 3,
    configured: true,
    ai_provider: 'Mock provider',
    ai_model: 'mock-flash',
    setup_url: '/ai-gateway',
    settings: {
      application_context: '',
      categories: [],
      property_keys: [],
      daily_enabled: false,
      share_activity_with_ai: false,
      min_sessions: 2,
      min_page_paths: 2,
    },
    settings_revision: 0,
    running: false,
    recent_runs: [],
    in_progress: null,
    cooldowns: {
      website_analysis_secs: null,
      preview_secs: null,
      report_secs: null,
    },
    ...overrides,
  } as ActivityStatus
}

function renderPage() {
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <MemoryRouter>
        <ActivityReportPage project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const modelNotFound = {
  title: 'AI provider error',
  detail:
    "The AI provider rejected the request: model 'mock-flash' not found — check AI Gateway settings.",
  code: 'provider_error',
  provider_failure: 'model_not_found',
  model: 'mock-flash',
  provider_status: 404,
  setup_url: '/ai-gateway',
}

test('a double-click sends one analysis, and a provider failure is shown with its fix and a retry', async () => {
  let goalRequests = 0
  mockApi({
    'GET /projects/7/environments': () => jsonResponse([environment]),
    'GET /projects/7/environments/3/domains': () => jsonResponse([]),
    'GET /projects/7/analytics/activity': () => jsonResponse(activityStatus()),
    'POST /projects/7/analytics/activity/goals': async () => {
      goalRequests += 1
      await new Promise((resolve) => setTimeout(resolve, 20))
      return jsonResponse(modelNotFound, 502)
    },
  })
  renderPage()
  const analyze = await screen.findByRole('button', { name: 'Analyze website' })
  await waitFor(() =>
    expect((analyze as HTMLButtonElement).disabled).toBe(false)
  )
  fireEvent.click(analyze)
  fireEvent.click(analyze)

  const alert = await screen.findByRole('alert', {}, { timeout: 3000 })
  await waitFor(() =>
    expect(alert.textContent).toContain("model 'mock-flash' not found")
  )
  expect(goalRequests).toBe(1)
  expect(screen.queryByText(/running or cooling down/)).toBeNull()
  expect(
    screen
      .getByRole('link', { name: 'Open AI Gateway settings' })
      .getAttribute('href')
  ).toBe('/ai-gateway')

  // The failure spent no tokens, so the retry goes straight back out.
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
  await waitFor(() => expect(goalRequests).toBe(2))
})

test('an analysis already running for the project blocks the button and shows its progress', async () => {
  let goalRequests = 0
  mockApi({
    'GET /projects/7/environments': () => jsonResponse([environment]),
    'GET /projects/7/environments/3/domains': () => jsonResponse([]),
    'GET /projects/7/analytics/activity': () =>
      jsonResponse(
        activityStatus({
          in_progress: {
            operation: 'website_analysis',
            started_at: new Date(Date.now() - 40_000).toISOString(),
          },
        })
      ),
    'POST /projects/7/analytics/activity/goals': () => {
      goalRequests += 1
      return jsonResponse({}, 500)
    },
  })
  renderPage()
  await waitFor(() =>
    expect(
      screen
        .getAllByRole('status')
        .some((node) =>
          /Website analysis is running for this project · started 4\ds ago/.test(
            node.textContent ?? ''
          )
        )
    ).toBe(true)
  )
  const analyze = screen.getByRole('button', { name: 'Analyze website' })
  expect((analyze as HTMLButtonElement).disabled).toBe(true)
  fireEvent.click(analyze)
  expect(goalRequests).toBe(0)
})

test('a cooldown after a successful analysis counts down on the button row', async () => {
  mockApi({
    'GET /projects/7/environments': () => jsonResponse([environment]),
    'GET /projects/7/environments/3/domains': () => jsonResponse([]),
    'GET /projects/7/analytics/activity': () =>
      jsonResponse(
        activityStatus({
          cooldowns: {
            website_analysis_secs: 25,
            preview_secs: null,
            report_secs: null,
          },
        })
      ),
  })
  renderPage()
  await waitFor(() =>
    expect(
      screen
        .getAllByRole('status')
        .some((node) =>
          /Website analysis is available again in 2[45]s/.test(
            node.textContent ?? ''
          )
        )
    ).toBe(true)
  )
  expect(
    (
      screen.getByRole('button', {
        name: 'Analyze website',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
})
