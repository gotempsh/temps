// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { listWorkflowGrantsOptions } from '@/api/client/@tanstack/react-query.gen'
import { WorkflowGrants } from './WorkflowGrants'

test('shows grant setup when no caller has access', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const options = listWorkflowGrantsOptions({
    path: { project_id: 1, environment_id: 2 },
    query: { page: 1, page_size: 20 },
  })
  client.setQueryData(options.queryKey, {
    grants: [],
    total: 0,
    total_pages: 1,
    page: 1,
    page_size: 20,
  })
  const html = renderToStaticMarkup(
    <MemoryRouter initialEntries={['/workflows/1/2/grants']}>
      <QueryClientProvider client={client}>
        <Routes>
          <Route
            path="/workflows/:projectId/:environmentId/grants"
            element={<WorkflowGrants />}
          />
        </Routes>
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('No external workflow access')
  expect(html).toContain('Grant access')
  expect(html).toContain('Run results stay private')
  expect(html).toContain('Source environment ID')
})
