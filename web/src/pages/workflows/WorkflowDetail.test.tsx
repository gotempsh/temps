// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listWorkflowRunsOptions } from '@/api/client/@tanstack/react-query.gen'
import { WorkflowDetail } from './WorkflowDetail'

test('workflow view queries a scoped name and links its executions', () => {
  const client = new QueryClient()
  client.setQueryData<object>(
    listWorkflowRunsOptions({
      path: { project_id: 1, environment_id: 2 },
      query: { workflow: 'email/sequence', page: 1, page_size: 20 },
    }).queryKey,
    {
      runs: [
        {
          id: 'synthetic-run',
          project_id: 1,
          environment_id: 2,
          workflow: 'email/sequence',
          status: 'completed',
          created_at: '2026-01-01T00:00:00Z',
          step_count: 3,
        },
      ],
      total: 1,
      total_pages: 1,
    }
  )
  const html = renderToStaticMarkup(
    <MemoryRouter initialEntries={['/workflows/1/2?workflow=email%2Fsequence']}>
      <QueryClientProvider client={client}>
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
  expect(html).toContain('email/sequence')
  expect(html).toContain('/workflows/1/2/runs/synthetic-run')
  expect(html).toContain('completed')
  expect(html).toContain('/workflows/1/2/grants')
})
