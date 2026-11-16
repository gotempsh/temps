// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { cleanup, fireEvent, render as renderDom } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import {
  RetainedFailedContainers,
  RetainedFailedContainersLoadError,
} from './RetainedFailedContainers'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { listContainerHistoryOptions } from '@/api/client/@tanstack/react-query.gen'
import { MemoryRouter } from 'react-router'

describe('RetainedFailedContainersLoadError', () => {
  test('does not hide a retained-container lookup failure', () => {
    const markup = renderToStaticMarkup(
      <RetainedFailedContainersLoadError onRetry={() => undefined} />
    )

    expect(markup).toContain('Could not load retained deployment containers')
    expect(markup).toContain('Retry')
  })
})

describe('retained startup crash logs', () => {
  function render(showLogsInitially: boolean) {
    const client = new QueryClient()
    const options = listContainerHistoryOptions({
      path: { project_id: 1, environment_id: 2 },
      query: { deployment_id: 3, limit: 20 },
    })
    client.setQueryData(options.queryKey, {
      total_count: 2,
      containers: [
        {
          id: 4,
          container_id: 'failed-app',
          container_name: 'app',
          deployed_at: '2026-01-01T00:00:00Z',
          deployment_id: 3,
          is_current: true,
        },
        {
          id: 5,
          container_id: 'other-app',
          container_name: 'other',
          deployed_at: '2026-01-01T00:00:00Z',
          deployment_id: 3,
          is_current: true,
        },
      ],
    })
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <RetainedFailedContainers
            projectId={1}
            projectSlug="app"
            environmentId={2}
            deploymentId={3}
            deploymentStatus="failed"
            showLogsInitially={showLogsInitially}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
  }

  test('opens the first retained log stream for startup crashes', () => {
    const html = render(true)
    expect(html).toContain('aria-label="Live logs for app"')
    expect(html).toContain('Hide logs')
    expect(html).not.toContain('aria-label="Live logs for other"')
  })

  test('keeps other failures collapsed until requested', () => {
    expect(render(false)).not.toContain('aria-label="Live logs for app"')
  })
})

describe('retained log choices per deployment', () => {
  installBrowserWindow()
  afterEach(cleanup)

  test("hiding one deployment's logs does not hide the next deployment's crash logs", () => {
    const client = new QueryClient({
      defaultOptions: { queries: { staleTime: Infinity } },
    })
    for (const deploymentId of [3, 4]) {
      const options = listContainerHistoryOptions({
        path: { project_id: 1, environment_id: 2 },
        query: { deployment_id: deploymentId, limit: 20 },
      })
      client.setQueryData(options.queryKey, {
        total_count: 1,
        containers: [
          {
            id: deploymentId,
            container_id: `failed-app-${deploymentId}`,
            container_name: `app-${deploymentId}`,
            deployed_at: '2026-01-01T00:00:00Z',
            deployment_id: deploymentId,
            is_current: true,
          },
        ],
      })
    }
    const view = (deploymentId: number) => (
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <RetainedFailedContainers
            projectId={1}
            projectSlug="app"
            environmentId={2}
            deploymentId={deploymentId}
            deploymentStatus="failed"
            showLogsInitially
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const screen = renderDom(view(3))
    fireEvent.click(screen.getByRole('button', { name: /Hide logs/ }))
    expect(
      screen.queryByRole('region', { name: 'Live logs for app-3' })
    ).toBeNull()

    screen.rerender(view(4))
    expect(
      screen.getByRole('region', { name: 'Live logs for app-4' })
    ).toBeTruthy()
  })
})
