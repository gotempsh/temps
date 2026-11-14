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

test('an environment without runs exposes onboarding, grants, keys and drain settings', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getWorkflowsOverviewOptions({ path: { project_id: 1, environment_id: 2 } })
      .queryKey,
    {
      executor: {
        replicas: 0,
        configured: false,
        reason: 'No deployment is live',
        setup_path: '/docs/workflows/getting-started',
      },
      credential: { configured: false },
      workflows: [],
    }
  )
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <WorkflowEnvironment projectId={1} environmentId={2} />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('No deployment is live')
  expect(html).not.toContain('/docs/workflows/getting-started')
  expect(html).toContain('href="/workflows/1/2/grants"')
  expect(html).toContain('href="/workflows/1/2/settings"')
  expect(html).toContain('href="/workflows/1/keys"')
  expect(html).toContain('Create credential')
  expect(html).toContain('Runs stay queued')
  expect(html).toContain('Start run')
  expect(html).toContain('role="combobox"')
  expect(html).toContain('No workflows are available')
  expect(html).not.toContain('placeholder="orders"')
  expect(html).not.toContain('Copy this credential now')
})

test('the selector offers current workflows and excludes historical names', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getWorkflowsOverviewOptions({ path: { project_id: 1, environment_id: 2 } })
      .queryKey,
    {
      executor: { configured: true, replicas: 1 },
      credential: { configured: true },
      workflows: [
        {
          name: 'send-email',
          defined: true,
          active_runs: 0,
          completed_runs: 0,
          failed_runs: 0,
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
          completed_runs: 0,
          failed_runs: 0,
        },
      ],
    }
  )
  try {
    const view = render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <WorkflowEnvironment projectId={1} environmentId={2} />
        </QueryClientProvider>
      </MemoryRouter>
    )
    const start = view.getByRole('button', { name: 'Start run' })
    expect(start.getAttribute('aria-disabled')).toBe('true')
    fireEvent.keyDown(view.getByRole('combobox'), { key: 'ArrowDown' })
    const option = await view.findByRole('option', { name: 'send-email' })
    expect(view.getByRole('option', { name: 'render-video' })).toBeTruthy()
    expect(view.queryByRole('option', { name: 'retired-workflow' })).toBeNull()
    fireEvent.click(option)
    expect(start.getAttribute('aria-disabled')).toBe('false')
  } finally {
    cleanup()
    client.clear()
  }
})
