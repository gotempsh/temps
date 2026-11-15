// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import type { AgentRunResponse, ProjectResponse } from '@/api/client'
import { latestRunForSourceOptions } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { AutofixRedirect } from './AutofixRedirect'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 7, slug: 'web' } as ProjectResponse

function Where() {
  const { pathname, search, hash } = useLocation()
  return <output data-testid="where">{`${pathname}${search}${hash}`}</output>
}

function renderAt(entry: string, client: QueryClient) {
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/projects/web">
            <Route
              path="errors/:errorGroupId/autofix"
              element={<AutofixRedirect project={project} />}
            />
            <Route path="errors" element={<Where />} />
            <Route path="agents/:runId" element={<Where />} />
          </Route>
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an invalid error group keeps the query string and hash on the way to errors', () => {
  const view = renderAt(
    '/projects/web/errors/0/autofix?a=b#c',
    new QueryClient()
  )
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/errors?a=b#c'
  )
})

test('an existing run keeps the query string and hash on the way to the run', () => {
  const client = new QueryClient()
  client.setQueryData(
    latestRunForSourceOptions({
      path: { project_id: project.id },
      query: { trigger_source_type: 'error_group', trigger_source_id: 5 },
    }).queryKey,
    { id: 42 } as AgentRunResponse
  )
  const view = renderAt('/projects/web/errors/5/autofix?a=b#c', client)
  expect(view.getByTestId('where').textContent).toBe(
    '/projects/web/agents/42?a=b#c'
  )
})
