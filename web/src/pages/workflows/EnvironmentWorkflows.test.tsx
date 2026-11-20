// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { render, fireEvent, cleanup } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { getWorkflowsOverviewOptions } from '@/api/client/@tanstack/react-query.gen'
import { WorkflowEnvironment } from './EnvironmentWorkflows'

installBrowserWindow()

function clientWith(overview: object) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData<object>(
    getWorkflowsOverviewOptions({ path: { project_id: 1, environment_id: 2 } })
      .queryKey,
    overview
  )
  return client
}

const definitions = {
  executor: { configured: true, replicas: 2, sdk: 'temps-workflows-node' },
  credential: {
    configured: true,
    prefix: 'twf_abc',
    last_used_at: '2026-01-01T00:00:00Z',
  },
  workflows: [
    {
      name: 'send-email',
      version: 'v3',
      defined: true,
      active_runs: 1,
      completed_runs: 12,
      failed_runs: 2,
      last_run_at: '2026-01-01T00:00:00Z',
    },
    {
      name: 'render-video',
      defined: true,
      active_runs: 0,
      completed_runs: 0,
      failed_runs: 0,
    },
    {
      name: 'retired-workflow',
      defined: false,
      active_runs: 0,
      completed_runs: 4,
      failed_runs: 0,
    },
  ],
}

test('an environment without workflows shows how to connect an app, step by step', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider
        client={clientWith({
          executor: {
            replicas: 0,
            configured: false,
            reason: 'No deployment is live',
            setup_path: '/docs/workflows/getting-started',
          },
          credential: { configured: false },
          workflows: [],
        })}
      >
        <WorkflowEnvironment projectId={1} environmentId={2} />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('Connect your app to durable workflows')
  expect(html).toContain('Create a workflow credential')
  expect(html).toContain('Create credential')
  expect(html).toContain('Define a workflow and serve it')
  expect(html).toContain('defineWorkflow')
  // Step 3 says why Temps cannot reach the app, and how to run it locally.
  expect(html).toContain('No deployment is live')
  expect(html).toContain(
    'temps workflows dev --project-id 1 --environment-id 2 --port 3000'
  )
  expect(html).toContain('Start a run')
  expect(html).not.toContain('/docs/workflows/getting-started')
  // Nothing to start yet, so no Start run action in the header.
  expect(html).not.toContain('>Start run<')
  expect(html).toContain('Workflow settings and access')
  expect(html).not.toContain('Copy the credential now')
})

test('a configured credential and a reachable app are marked done in the guide', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider
        client={clientWith({
          executor: { replicas: 1, configured: true },
          credential: { configured: true, prefix: 'twf_abc' },
          workflows: [],
        })}
      >
        <WorkflowEnvironment projectId={1} environmentId={2} />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('Rotate credential')
  expect(html).toContain('Temps can reach your workflows.')
  expect(html.match(/\(done\)/g)?.length).toBe(2)
})

test('workflows are listed with their state and counts and open their runs', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={clientWith(definitions)}>
        <WorkflowEnvironment projectId={1} environmentId={2} />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('href="/workflows/1/2?workflow=send-email"')
  expect(html).toContain('Version v3')
  expect(html).toContain('Deployed')
  expect(html).toContain('Not in deployment')
  expect(html).toContain('Never')
  expect(html).toContain('Executor reachable')
  expect(html).toContain('temps-workflows-node · 2 replicas')
  expect(html).toContain('twf_abc…')
  expect(html).not.toContain('Connect your app to durable workflows')
  // One Run action per workflow the deployment still declares.
  expect(html.match(/>Run</g)?.length).toBe(2)
})

test('an unreachable executor is flagged above the list with a link to its settings', () => {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider
        client={clientWith({
          ...definitions,
          executor: {
            configured: false,
            replicas: 0,
            reason: 'The deployment does not answer',
          },
        })}
      >
        <WorkflowEnvironment projectId={1} environmentId={2} />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('Runs stay queued until Temps can reach your app')
  expect(html).toContain('The deployment does not answer')
  expect(html).toContain('href="/workflows/1/2/settings"')
})

test('settings, grants and keys are one menu away', async () => {
  const client = clientWith(definitions)
  try {
    const view = render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <WorkflowEnvironment projectId={1} environmentId={2} />
        </QueryClientProvider>
      </MemoryRouter>
    )
    fireEvent.keyDown(
      view.getByRole('button', { name: 'Workflow settings and access' }),
      { key: 'Enter' }
    )
    const settings = await view.findByRole('menuitem', { name: 'Settings' })
    expect(settings.getAttribute('href')).toBe('/workflows/1/2/settings')
    expect(
      view.getByRole('menuitem', { name: 'Access grants' }).getAttribute('href')
    ).toBe('/workflows/1/2/grants')
    expect(
      view
        .getByRole('menuitem', { name: 'Encryption keys' })
        .getAttribute('href')
    ).toBe('/workflows/1/keys')
    expect(
      view.getByRole('menuitem', { name: 'Rotate credential' })
    ).toBeTruthy()
  } finally {
    cleanup()
    client.clear()
  }
})

test('until the credential status loads, the menu cannot create or rotate one', async () => {
  // Nothing cached: the overview is still loading.
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryDefaults(
    getWorkflowsOverviewOptions({ path: { project_id: 1, environment_id: 2 } })
      .queryKey,
    { queryFn: () => new Promise(() => {}) }
  )
  try {
    const view = render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <WorkflowEnvironment projectId={1} environmentId={2} />
        </QueryClientProvider>
      </MemoryRouter>
    )
    fireEvent.keyDown(
      view.getByRole('button', { name: 'Workflow settings and access' }),
      { key: 'Enter' }
    )
    const item = await view.findByRole('menuitem', {
      name: 'Checking credential…',
    })
    expect(item.hasAttribute('data-disabled')).toBe(true)
    expect(
      view.queryByRole('menuitem', { name: 'Create credential' })
    ).toBeNull()
  } finally {
    cleanup()
    client.clear()
  }
})

test('starting a run offers only the workflows the deployment declares', async () => {
  const client = clientWith(definitions)
  try {
    const view = render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <WorkflowEnvironment projectId={1} environmentId={2} />
        </QueryClientProvider>
      </MemoryRouter>
    )
    fireEvent.click(view.getByRole('button', { name: 'Run render-video' }))
    const dialog = await view.findByRole('dialog', { name: 'Start a run' })
    const picker = dialog.querySelector('[role="combobox"]')
    expect(picker?.textContent).toContain('render-video')
    fireEvent.keyDown(picker as Element, { key: 'ArrowDown' })
    expect(await view.findByRole('option', { name: 'send-email' })).toBeTruthy()
    expect(view.queryByRole('option', { name: 'retired-workflow' })).toBeNull()
  } finally {
    cleanup()
    client.clear()
  }
})
