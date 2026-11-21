// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { installBrowserWindow } from '@/test/dom'
import { Link, MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  getWorkflowSettingsOptions,
  getWorkflowsOverviewOptions,
  listWorkflowRunsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { WorkflowDetail } from './WorkflowDetail'
import {
  openTabQueryKeys,
  workflowDetailCrumbs,
  workflowDetailTab,
  workflowGrantsOptions,
  workflowKeysOptions,
} from './detail-tabs'

installBrowserWindow()
afterEach(cleanup)

const path = { project_id: 1, environment_id: 2 }

function client() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function withRuns(workflow: string, runs: object[]) {
  const cache = client()
  cache.setQueryData<object>(
    listWorkflowRunsOptions({
      path,
      query: { workflow, page: 1, page_size: 20 },
    }).queryKey,
    { runs, total: runs.length, total_pages: runs.length ? 1 : 0 }
  )
  return cache
}

function page(cache: QueryClient, url: string) {
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={[url]}>
      <QueryClientProvider client={cache}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/workflows/:projectId/:environmentId"
              element={<WorkflowDetail />}
            />
          </Routes>
        </BreadcrumbProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

const completed = {
  id: 'synthetic-run',
  project_id: 1,
  environment_id: 2,
  workflow: 'email/sequence',
  status: 'completed',
  created_at: '2026-01-01T00:00:00Z',
  step_count: 3,
}

test('workflow view queries a scoped name and opens each run from its row', () => {
  const html = page(
    withRuns('email/sequence', [completed]),
    '/workflows/1/2?workflow=email%2Fsequence'
  )
  expect(html).toContain('email/sequence')
  expect(html).toContain('/workflows/1/2/runs/synthetic-run')
  expect(html).toContain('completed')
})

test('settings, access and keys are tabs on the page, not a menu', () => {
  const html = page(withRuns('orders', []), '/workflows/1/2?workflow=orders')
  for (const tab of ['Runs', 'Settings', 'Access', 'Encryption keys'])
    expect(html).toMatch(new RegExp(`role="tab"[^>]*>[\\s\\S]*?${tab}`))
  expect(html).not.toContain('Workflow settings and access')
})

test('the header says whether the workflow is deployed and how it is doing', () => {
  const cache = withRuns('orders', [])
  cache.setQueryData<object>(getWorkflowsOverviewOptions({ path }).queryKey, {
    executor: { configured: true, replicas: 1 },
    credential: { configured: true },
    workflows: [
      {
        name: 'orders',
        version: 'v4',
        defined: true,
        concurrency: 5,
        active_runs: 2,
        completed_runs: 30,
        failed_runs: 1,
        last_run_at: null,
      },
    ],
  })
  const html = page(cache, '/workflows/1/2?workflow=orders')
  expect(html).toContain('Deployed')
  expect(html).toContain('v4')
  expect(html).toContain('Completed')
  expect(html).toContain('>30<')
  expect(html).toContain('Never')
})

test('the settings tab shows the environment settings form in place', () => {
  const cache = withRuns('orders', [])
  cache.setQueryData<object>(getWorkflowSettingsOptions({ path }).queryKey, {
    drain_timeout_secs: 0,
    host_service: null,
  })
  const html = page(cache, '/workflows/1/2?workflow=orders&tab=settings')
  expect(html).toContain('Journal storage')
  expect(html).toContain('Applies to every workflow in this environment')
  expect(html).not.toContain('No runs yet')
})

test('the access tab offers the grant form with the grants under it', () => {
  const cache = withRuns('orders', [])
  cache.setQueryData<object>(workflowGrantsOptions(path, 1).queryKey, {
    grants: [
      {
        id: 7,
        source_environment_id: 9,
        action: 'start',
        resource: 'orders',
        created_at: '2026-01-01T00:00:00Z',
      },
    ],
    total: 1,
    total_pages: 1,
    page: 1,
    page_size: 20,
  })
  const html = page(cache, '/workflows/1/2?workflow=orders&tab=access')
  expect(html).toContain('Grant access')
  expect(html).toContain('Start workflow')
  expect(html).toContain('Revoke')
})

test('the keys tab lists the project keys', () => {
  const cache = withRuns('orders', [])
  cache.setQueryData<object>(workflowKeysOptions(1, 1).queryKey, {
    keys: [
      { id: 'key-1', created_at: '2026-01-01T00:00:00Z', shredded_at: null },
    ],
    total: 1,
    total_pages: 1,
    page: 1,
    page_size: 20,
  })
  const html = page(cache, '/workflows/1/2?workflow=orders&tab=keys')
  expect(html).toContain('key-1')
  expect(html).toContain('Shred key')
})

test('a workflow with no runs says how to start one', () => {
  const html = page(withRuns('orders', []), '/workflows/1/2?workflow=orders')
  expect(html).toContain('No runs yet')
  expect(html).toContain('Start orders from your app')
})

test('an unknown tab opens the runs', () => {
  expect(workflowDetailTab('access')).toBe('access')
  expect(workflowDetailTab('nope')).toBe('runs')
  expect(workflowDetailTab(null)).toBe('runs')
})

test('on a tab the workflow crumb links back to the workflow', () => {
  const base = {
    projectId: 1,
    environmentId: 2,
    workflow: 'email/sequence',
    project: { name: 'Shop', slug: 'shop' },
    environment: { name: 'Production', slug: 'production' },
  }
  const onRuns = workflowDetailCrumbs({ ...base, tab: 'runs' })
  // On Runs the workflow is the current page, so it is the last crumb.
  expect(onRuns[onRuns.length - 1].label).toBe('email/sequence')
  expect(onRuns[2].href).toBe('/projects/shop/durable-workflows?env=production')

  const onSettings = workflowDetailCrumbs({ ...base, tab: 'settings' })
  expect(onSettings.map((crumb) => crumb.label)).toEqual([
    'Workflows',
    'Shop',
    'Production',
    'email/sequence',
    'Settings',
  ])
  expect(onSettings[3].href).toBe('/workflows/1/2?workflow=email%2Fsequence')
})

test('an unsaved access edit survives switching to another tab and back', () => {
  const cache = withRuns('orders', [])
  cache.setQueryData<object>(workflowGrantsOptions(path, 1).queryKey, {
    grants: [],
    total: 0,
    total_pages: 0,
    page: 1,
    page_size: 20,
  })
  const view = render(
    <MemoryRouter
      initialEntries={['/workflows/1/2?workflow=orders&tab=access']}
    >
      <QueryClientProvider client={cache}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/workflows/:projectId/:environmentId"
              element={<WorkflowDetail />}
            />
          </Routes>
        </BreadcrumbProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
  const resource = () =>
    view.container.querySelector<HTMLInputElement>('#workflow-resource')
  fireEvent.input(resource()!, { target: { value: 'orders-draft' } })
  fireEvent.mouseDown(view.getByRole('tab', { name: /Runs/ }))
  expect(view.getByRole('tab', { name: /Runs/ }).dataset.state).toBe('active')
  // Hidden, not removed: the draft is still in the form.
  expect(resource()?.value).toBe('orders-draft')
  fireEvent.mouseDown(view.getByRole('tab', { name: /Access/ }))
  expect(resource()?.value).toBe('orders-draft')
  // A tab never opened loads nothing.
  expect(view.container.textContent).not.toContain('Journal storage')
})

test('an unsaved access edit stays with its environment', () => {
  const cache = withRuns('orders', [])
  const other = { project_id: 1, environment_id: 3 }
  for (const grantsPath of [path, other])
    cache.setQueryData<object>(workflowGrantsOptions(grantsPath, 1).queryKey, {
      grants: [],
      total: 0,
      total_pages: 0,
      page: 1,
      page_size: 20,
    })
  const view = render(
    <MemoryRouter
      initialEntries={['/workflows/1/2?workflow=orders&tab=access']}
    >
      <QueryClientProvider client={cache}>
        <BreadcrumbProvider>
          <Link to="/workflows/1/3?workflow=orders&tab=access">Staging</Link>
          <Routes>
            <Route
              path="/workflows/:projectId/:environmentId"
              element={<WorkflowDetail />}
            />
          </Routes>
        </BreadcrumbProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
  const resource = () =>
    view.container.querySelector<HTMLInputElement>('#workflow-resource')
  fireEvent.input(resource()!, { target: { value: 'orders-draft' } })
  fireEvent.click(view.getByRole('link', { name: 'Staging' }))
  // The page stays mounted across environments; the form starts over.
  expect(resource()?.value).toBe('')
})

test('refresh reloads the list on the open tab, never the settings form', () => {
  expect(openTabQueryKeys('access', path, 2)).toEqual([
    workflowGrantsOptions(path, 2).queryKey,
  ])
  expect(openTabQueryKeys('keys', path, 3)).toEqual([
    workflowKeysOptions(path.project_id, 3).queryKey,
  ])
  expect(openTabQueryKeys('settings', path, 1)).toEqual([])
  // Settings that failed to load say "Refresh to retry", so Refresh retries.
  expect(openTabQueryKeys('settings', path, 1, true)).toEqual([
    getWorkflowSettingsOptions({ path }).queryKey,
  ])
  expect(openTabQueryKeys('runs', path, 1)).toEqual([])
})
