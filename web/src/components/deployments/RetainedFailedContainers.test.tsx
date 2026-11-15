// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
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
