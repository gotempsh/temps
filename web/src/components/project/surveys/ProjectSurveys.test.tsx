// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse, SurveyListResponse } from '@/api/client'
import { listSurveysOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectSurveys } from './ProjectSurveys'

const project = {
  id: 1,
  slug: 'sample-app',
  name: 'sample-app',
} as unknown as ProjectResponse
const surveysKey = listSurveysOptions({
  path: { project_id: 1 },
  query: { page: 1 },
}).queryKey
const emptyPage: SurveyListResponse = {
  surveys: [],
  page: 1,
  page_size: 20,
  total: 0,
  total_pages: 1,
}

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}
function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  const query = client.getQueryCache().build(client, { queryKey })
  query.setState({
    ...query.state,
    status: 'error',
    error: error as Error,
    fetchStatus: 'idle',
  })
}
function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProjectSurveys project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an empty project onboards with an example and the SDK it needs', () => {
  const client = createClient()
  client.setQueryData(surveysKey, emptyPage)
  const html = render(client)
  expect(html).toContain('No surveys yet')
  expect(html).toContain('How likely are you to recommend us?')
  expect(html).toContain('TempsSurveys')
  expect(html).toContain('Create a survey')
  client.clear()
})

test('a failed read is not an empty project', () => {
  const client = createClient()
  fail(client, surveysKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires SurveysRead permission',
  })
  const html = render(client)
  expect(html).toContain('Surveys: access denied')
  expect(html).toContain('Requires SurveysRead permission')
  expect(html).not.toContain('No surveys yet')
  client.clear()
})

test('lists surveys with their status and when they show', () => {
  const client = createClient()
  client.setQueryData(surveysKey, {
    ...emptyPage,
    total: 1,
    surveys: [
      {
        id: 4,
        project_id: 1,
        name: 'Checkout feedback',
        description: null,
        status: 'active',
        questions: [{ id: 'nps', kind: 'nps', prompt: 'Recommend us?' }],
        targeting: { path_pattern: '/checkout/*', sample_rate: 0.5 },
        created_at: '2026-11-20T00:00:00Z',
        updated_at: '2026-11-20T00:00:00Z',
      },
    ],
  })
  const html = render(client)
  expect(html).toContain('Checkout feedback')
  expect(html).toContain('Active')
  expect(html).toContain('On /checkout/*, 50% of visitors, once per visitor')
  expect(html).not.toContain('No surveys yet')
  client.clear()
})
