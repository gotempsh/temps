// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { listWorkflowProjectKeysOptions } from '@/api/client/@tanstack/react-query.gen'
import type { WorkflowProjectKeyResponse } from '@/api/client'
import { ProjectWorkflowKeys } from './ProjectWorkflowKeys'

function render(keys: WorkflowProjectKeyResponse[]) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const options = listWorkflowProjectKeysOptions({
    path: { project_id: 1 },
    query: { page: 1, page_size: 20 },
  })
  client.setQueryData(options.queryKey, {
    keys,
    total: keys.length,
    total_pages: 1,
    page: 1,
    page_size: 20,
  })
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={['/workflows/1/keys']}>
      <QueryClientProvider client={client}>
        <Routes>
          <Route
            path="/workflows/:projectId/keys"
            element={<ProjectWorkflowKeys />}
          />
        </Routes>
      </QueryClientProvider>
    </MemoryRouter>
  )
}
test('explains first-use setup and the backup limitation', () => {
  const html = render([])
  expect(html).toContain('No project key yet')
  expect(html).toContain('automatically')
  expect(html).toContain('Backups containing the key can restore access')
  expect(html).toContain('Legacy payloads use the instance key')
})
test('offers shredding only for an active key', () => {
  const html = render([
    { id: 'active-key', created_at: '2026-01-01T00:00:00Z', shredded_at: null },
    {
      id: 'old-key',
      created_at: '2026-01-01T00:00:00Z',
      shredded_at: '2026-01-02T00:00:00Z',
    },
  ])
  expect(html).toContain('Active')
  expect(html).toContain('Shredded')
  expect(html.match(/>Shred key</g)?.length).toBe(1)
})
