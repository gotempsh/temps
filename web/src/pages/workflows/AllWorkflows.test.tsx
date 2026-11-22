// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { listWorkflowDefinitionsOptions } from '@/api/client/@tanstack/react-query.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AllWorkflows } from './AllWorkflows'
import { definitionStatus, workflowTone } from './status'

const definitionsKey = listWorkflowDefinitionsOptions({
  query: { page: 1, page_size: 20 },
}).queryKey

function page(client: QueryClient, url = '/workflows') {
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={[url]}>
      <QueryClientProvider client={client}>
        <BreadcrumbProvider>
          <AllWorkflows />
        </BreadcrumbProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

function withDefinitions(workflows: object[]) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData<object>(definitionsKey, {
    workflows,
    total: workflows.length,
    page: 1,
    page_size: 20,
    total_pages: workflows.length ? 1 : 0,
  })
  return client
}

function withRuns(runs: object[], nextCursor: string | null = null) {
  const client = withDefinitions([])
  client.setQueryData(['all-workflow-runs', undefined], {
    pages: [{ runs, next_cursor: nextCursor }],
    pageParams: [undefined],
  })
  return client
}

function failedRuns(error: object, runs?: object[]) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData<object>(definitionsKey, {
    workflows: [],
    total: 0,
    page: 1,
    page_size: 20,
    total_pages: 0,
  })
  const query = client.getQueryCache().build<unknown, object>(client, {
    queryKey: ['all-workflow-runs', undefined],
  })
  query.setState({
    status: 'error',
    fetchStatus: 'idle',
    error,
    data: runs
      ? { pages: [{ runs, next_cursor: null }], pageParams: [undefined] }
      : undefined,
  })
  return client
}

const run = {
  id: 'synthetic-run-0001',
  project_id: 1,
  environment_id: 2,
  project_slug: 'shop',
  environment_slug: 'production',
  workflow: 'orders',
  status: 'waiting',
  created_at: '2026-01-01T00:00:00Z',
  step_count: 3,
}

describe('Workflows', () => {
  test('lists each workflow once, by project and environment, and opens its runs', () => {
    const html = page(
      withDefinitions([
        {
          name: 'orders',
          project_id: 1,
          environment_id: 2,
          project_slug: 'shop',
          environment_slug: 'production',
          state: 'available',
          version: 'v2',
          active_runs: 3,
          completed_runs: 40,
          failed_runs: 1,
          last_run_at: '2026-01-01T00:00:00Z',
        },
        {
          name: 'refunds',
          project_id: 1,
          environment_id: 2,
          project_slug: 'shop',
          environment_slug: 'production',
          state: 'unknown',
          active_runs: 0,
          completed_runs: 0,
          failed_runs: 0,
        },
      ])
    )
    expect(html).toContain('href="/workflows/1/2?workflow=orders"')
    expect(html).toContain('shop · production · Version v2')
    expect(html).toContain('Deployed')
    expect(html).toContain('Not checked yet')
    expect(html).toContain('Never')
    // The tab counts the workflows, not the runs.
    expect(html).toMatch(/Workflows[\s\S]*?>2</)
    expect(html).not.toContain('No workflows yet')
  })

  test('with no workflows the page shows how to connect an app', () => {
    const html = page(withDefinitions([]))
    expect(html).toContain('No workflows yet')
    expect(html).toContain('Define a workflow and serve it')
    expect(html).toContain('href="/projects"')
    expect(html).toContain('--project-id &lt;project-id&gt;')
  })

  test('runs name their project and environment and offer continuation', () => {
    const html = page(withRuns([run], 'opaque'), '/workflows?view=runs')
    expect(html).toContain('/workflows/1/2/runs/synthetic-run-0001')
    expect(html).toContain('shop · production')
    expect(html).toContain('synthetic')
    expect(html).toContain('waiting')
    expect(html).toContain('Load more runs')
    expect(html).not.toContain('opaque')
  })

  test('no runs yet points to the setup guide', () => {
    const html = page(withRuns([]), '/workflows?view=runs')
    expect(html).toContain('No workflow runs yet')
    expect(html).toContain('See how to connect an app')
  })

  test('a failed load says why once and offers a retry', () => {
    const html = page(
      failedRuns({ title: 'Not Found', status: 404 }),
      '/workflows?view=runs'
    )
    expect(html).toContain('Could not load workflow runs')
    expect(html).toContain('The server answered: Not Found')
    expect(html).toContain('Try again')
    expect(html).not.toContain('role="alert"')
  })

  test('a failed refresh keeps the loaded runs and flags the failure inline', () => {
    const html = page(
      failedRuns({ detail: 'Database unavailable' }, [
        { ...run, status: 'running' },
      ]),
      '/workflows?view=runs'
    )
    expect(html).toContain('orders')
    expect(html).toContain('role="alert"')
    expect(html).toContain(
      'Could not refresh workflow runs: Database unavailable'
    )
    expect(html).not.toContain('Could not load workflow runs')
  })

  test('a status filter in the link opens the runs', () => {
    const client = withDefinitions([])
    client.setQueryData(['all-workflow-runs', 'failed'], {
      pages: [{ runs: [{ ...run, status: 'failed' }], next_cursor: null }],
      pageParams: [undefined],
    })
    const html = page(client, '/workflows?status=failed')
    expect(html).toContain('/workflows/1/2/runs/synthetic-run-0001')
  })

  test('uses failure, waiting and definition states consistently', () => {
    expect(workflowTone('stuck')).toBe('error')
    expect(workflowTone('failed')).toBe('error')
    expect(workflowTone('waiting')).toBe('warn')
    expect(workflowTone('completed')).toBe('ok')
    expect(definitionStatus('available').tone).toBe('ok')
    expect(definitionStatus('missing').tone).toBe('warn')
    expect(definitionStatus('unknown').tone).toBe('idle')
  })
})
