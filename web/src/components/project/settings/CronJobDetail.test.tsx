// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import {
  getCronByIdOptions,
  getCronExecutionsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { CronExecutionInfo, CronInfo, ProjectResponse } from '@/api/client'
import { CronJobDetail } from './CronJobDetail'
import { executionStatus } from './cronExecutionStatus'

const path = { project_id: 1, env_id: 2, cron_id: 3 }
const cronOptions = getCronByIdOptions({ path })
const executionsOptions = getCronExecutionsOptions({
  path,
  query: { page: 1, per_page: 10 },
})
const cron: CronInfo = {
  id: 3,
  project_id: 1,
  environment_id: 2,
  path: '/tasks/cleanup',
  schedule: '0 * * * *',
  mode: 'http',
  concurrency_policy: 'forbid',
  retry_limit: 0,
  has_workflow_input: false,
  next_run: null,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
const execution: CronExecutionInfo = {
  id: 4,
  cron_id: 3,
  executed_at: '2026-01-02T00:00:00Z',
  headers: '{}',
  response_time_ms: 250,
  status_code: 200,
  url: 'https://example.test/tasks/cleanup',
}

function renderPanel({
  configuration = cron,
  executions = [execution],
  configurationError = false,
  executionsError = false,
}: {
  configuration?: CronInfo | null
  executions?: CronExecutionInfo[] | null
  configurationError?: boolean
  executionsError?: boolean
} = {}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  if (configuration) client.setQueryData(cronOptions.queryKey, configuration)
  if (executions) client.setQueryData(executionsOptions.queryKey, executions)
  if (configurationError) {
    client
      .getQueryCache()
      .build(client, { queryKey: cronOptions.queryKey })
      .setState({
        status: 'error',
        error: new Error('Configuration unavailable'),
        fetchStatus: 'idle',
      })
  }
  if (executionsError) {
    client
      .getQueryCache()
      .build(client, { queryKey: executionsOptions.queryKey })
      .setState({
        status: 'error',
        error: new Error('History unavailable'),
        fetchStatus: 'idle',
      })
  }
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/environments/2/crons/3']}>
        <Routes>
          <Route
            path="/environments/:environmentId/crons/:cronId"
            element={
              <CronJobDetail
                project={{ id: 1, slug: 'sample' } as ProjectResponse}
              />
            }
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

describe('CronJobDetail workflow crons', () => {
  test('shows the workflow a cron starts and links each execution to its run', () => {
    const markup = renderPanel({
      configuration: {
        ...cron,
        path: 'workflow:send-digest',
        mode: 'workflow',
        workflow: 'send-digest',
        has_workflow_input: true,
      },
      executions: [
        {
          ...execution,
          status_code: 0,
          state: 'running',
          workflow_run_id: '00000000-0000-4000-8000-0000000000aa',
        },
      ],
    })
    expect(markup).toContain('Starts workflow')
    expect(markup).toContain('send-digest')
    expect(markup).toContain('with the input set in .temps.yaml')
    expect(markup).toContain('00000000-0000-4000-8000-0000000000aa')
    expect(markup).toContain('Running')
    expect(markup).not.toContain('Status: 0')
  })

  test('reads a run state before the status code', () => {
    const at = (state: string | null, status_code: number) =>
      executionStatus({ ...execution, state, status_code })
    expect(at('running', 0)).toEqual({ tone: 'running', label: 'Running' })
    expect(at('skipped', 0)).toEqual({ tone: 'idle', label: 'Skipped' })
    expect(at('succeeded', 200)).toEqual({ tone: 'ok', label: 'Success' })
    expect(at('failed', 500).tone).toBe('error')
    expect(at(null, 204).tone).toBe('ok')
    expect(at(null, 503).tone).toBe('error')
  })
})

describe('CronJobDetail request states', () => {
  test('renders execution status, timing and failure details in the shared table', () => {
    const markup = renderPanel({
      executions: [
        execution,
        {
          ...execution,
          id: 5,
          status_code: 503,
          error_message: 'Service unavailable',
        },
      ],
    })
    expect(markup).toContain('<table')
    expect(markup).toContain('Success')
    expect(markup).toContain('Failed')
    expect(markup).toContain('250ms')
    expect(markup).toContain('503')
    expect(markup).toContain('Service unavailable')
    expect(markup).toContain('Not scheduled')
  })

  test('does not format a missing configuration date when the request fails', () => {
    const markup = renderPanel({
      configuration: null,
      configurationError: true,
    })
    expect(markup).toContain('Retry configuration')
    expect(markup).toContain('data-page-state="failed"')
    expect(markup).toContain('250ms')
    expect(markup).not.toContain('Invalid Date')
  })

  test.each([{ cached: false }, { cached: true }])(
    'does not report failed history as empty (cached=$cached)',
    ({ cached }) => {
      const markup = renderPanel({
        executions: cached ? [] : null,
        executionsError: true,
      })
      expect(markup).toContain('Retry executions')
      expect(markup).toContain('data-page-state="failed"')
      expect(markup).not.toContain('No executions yet')
      expect(markup).toContain('/tasks/cleanup')
    }
  )

  test('only shows empty history after a successful empty response', () => {
    const markup = renderPanel({ executions: [] })
    expect(markup).toContain('data-page-state="empty"')
    expect(markup).toContain('No executions yet')
    expect(markup).not.toContain('Retry executions')
  })

  test('retains configuration and rows after background refresh failures', () => {
    const markup = renderPanel({
      configurationError: true,
      executionsError: true,
    })
    expect(markup).toContain('Showing the last loaded configuration')
    expect(markup).toContain('Showing the last loaded executions')
    expect(markup).toContain('/tasks/cleanup')
    expect(markup).toContain('250ms')
    expect(markup).not.toContain('data-page-state="failed"')
  })

  test('keeps configuration visible while history loads', () => {
    const markup = renderPanel({ executions: null })
    expect(markup).toContain('/tasks/cleanup')
    expect(markup).toContain('<table')
    expect(markup).not.toContain('No executions yet')
  })
})
