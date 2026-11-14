// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  getProjectOptions,
  getEnvironmentOptions,
  getWorkflowRunOptions,
  listWorkflowStepsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { WorkflowRunDetail } from './WorkflowRunDetail'

function render(populated = true) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const path = { project_id: 1, environment_id: 2, run_id: 'synthetic-run' }
  if (populated) {
    client.setQueryData<object>(
      getProjectOptions({ path: { id: 1 } }).queryKey,
      {
        id: 1,
        name: 'Example app',
        slug: 'example-app',
      }
    )
    client.setQueryData<object>(
      getEnvironmentOptions({ path: { project_id: 1, env_id: 2 } }).queryKey,
      { name: 'Preview' }
    )
    client.setQueryData<object>(getWorkflowRunOptions({ path }).queryKey, {
      workflow: 'email/sequence',
      status: 'completed',
      created_at: '2026-01-01T00:00:00Z',
      attempt: 2,
      step_count: 1,
    })
    client.setQueryData<object>(
      listWorkflowStepsOptions({ path, query: { page: 1, page_size: 20 } })
        .queryKey,
      {
        steps: [
          {
            sequence: 0,
            name: 'send-email',
            kind: 'step',
            status: 'completed',
            attempts: 1,
          },
        ],
        total: 1,
        total_pages: 1,
      }
    )
  }
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={['/workflows/1/2/runs/synthetic-run']}>
      <QueryClientProvider client={client}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/workflows/:projectId/:environmentId/runs/:runId"
              element={<WorkflowRunDetail />}
            />
          </Routes>
        </BreadcrumbProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

test('run header resolves names, relative time and workflow navigation without a facts row or settings buttons', () => {
  const html = render()
  expect(html).toContain('Example app')
  expect(html).toContain('Preview')
  expect(html).toContain('ago')
  expect(html).toContain('Attempt 2')
  expect(html).toContain('/workflows/1/2?workflow=email%2Fsequence')
  expect(html).toContain('Copy run ID')
  expect(html).toContain('send-email')
  expect(html).not.toContain('<dl')
  expect(html).not.toContain('Encryption keys')
  expect(html).not.toContain('Access grants')
  expect(html).not.toContain('Showing 1')
})

test('loading metadata is explicit and does not present numeric IDs as names', () => {
  const html = render(false)
  expect(html).toContain('Loading project')
  expect(html).toContain('Loading environment')
  expect(html).not.toContain('Invalid Date')
})
