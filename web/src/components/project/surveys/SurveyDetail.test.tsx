// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import type {
  EnvironmentResponse,
  ProjectResponse,
  SurveyResponse,
} from '@/api/client'
import { client } from '@/api/client/client.gen'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from '@/components/project-frame/project-environment-context'
import { installBrowserWindow } from '@/test/dom'
import { SurveyDetail } from './SurveyDetail'

installBrowserWindow()
const realFetch = globalThis.fetch
const realBaseUrl = client.getConfig().baseUrl
afterEach(() => {
  cleanup()
  globalThis.fetch = realFetch
  client.setConfig({ baseUrl: realBaseUrl })
})

const project = {
  id: 1,
  slug: 'sample-app',
  name: 'sample-app',
} as unknown as ProjectResponse
const staging = {
  id: 5,
  name: 'staging',
  slug: 'staging',
} as EnvironmentResponse
const survey = {
  id: 3,
  project_id: 1,
  name: 'Checkout feedback',
  description: null,
  status: 'active',
  questions: [{ id: 'q_score', kind: 'nps', prompt: 'How likely?' }],
  targeting: { sample_rate: 1, once_per_visitor: true },
  created_at: '2026-11-20T09:00:00Z',
  updated_at: '2026-11-20T09:00:00Z',
} as SurveyResponse

function bar(environment: EnvironmentResponse | undefined) {
  return {
    environments: [staging],
    isPending: false,
    isError: false,
    retry: () => {},
    scope: 'all',
    selection: { environment, all: !environment, source: 'url' },
    select: () => {},
  } as unknown as ProjectEnvironmentContextValue
}

function body(url: URL): unknown {
  if (url.pathname.endsWith('/results'))
    return {
      start_date: '2026-10-21T00:00:00Z',
      end_date: '2026-11-20T00:00:00Z',
      shown: 0,
      sent: 0,
      dismissed: 0,
      response_rate: null,
      questions: [],
    }
  if (url.pathname.endsWith('/responses'))
    return { responses: [], page: 1, page_size: 20, total: 0, total_pages: 1 }
  return survey
}

test('results and responses follow the environment picked in the project bar', async () => {
  const requested: URL[] = []
  client.setConfig({ baseUrl: 'http://console.example.test/api' })
  globalThis.fetch = (async (request: Request) => {
    const url = new URL(request.url)
    requested.push(url)
    return new Response(JSON.stringify(body(url)), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }) as unknown as typeof fetch
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const tree = (environment: EnvironmentResponse | undefined) => (
    <QueryClientProvider client={queryClient}>
      <ProjectEnvironmentContext.Provider value={bar(environment)}>
        <MemoryRouter
          initialEntries={['/projects/sample-app/analytics/surveys/3']}
        >
          <Routes>
            <Route
              path="/projects/:slug/analytics/surveys/:surveyId"
              element={<SurveyDetail project={project} />}
            />
          </Routes>
        </MemoryRouter>
      </ProjectEnvironmentContext.Provider>
    </QueryClientProvider>
  )
  const scoped = (suffix: string) =>
    requested
      .filter((url) => url.pathname.endsWith(suffix))
      .map((url) => url.searchParams.get('environment_id'))

  const view = render(tree(staging))
  await waitFor(() => {
    expect(scoped('/results')).toEqual(['5'])
    expect(scoped('/responses')).toEqual(['5'])
  })

  // Back to every environment: a new request without the filter, not the
  // staging answers left on screen.
  view.rerender(tree(undefined))
  await waitFor(() => {
    expect(scoped('/results')).toEqual(['5', null])
    expect(scoped('/responses')).toEqual(['5', null])
  })
  queryClient.clear()
})
