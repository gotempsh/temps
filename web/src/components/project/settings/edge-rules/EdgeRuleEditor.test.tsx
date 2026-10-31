// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import type { ApiRule } from './api'
import { EdgeRuleEditor } from './EdgeRuleEditor'
import { edgeRulesQueryKey } from './store'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 1, slug: 'web' } as ProjectResponse

function apiRule(revision: number, name: string): ApiRule {
  return {
    id: 5,
    revision,
    name,
    enabled: true,
    mode: 'enforce',
    environment_id: null,
    expression: {
      combinator: 'all',
      children: [
        {
          kind: 'condition',
          field: 'path',
          op: 'starts_with',
          values: ['/admin'],
        },
      ],
    },
    action: { type: 'block', status: 403 },
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    hits: 0,
  }
}

test('saving sends the revision the form was loaded from, not a refetched one', async () => {
  const patches: Array<Record<string, unknown>> = []
  // What the server holds. Someone else saves the rule once the form is open.
  let current = apiRule(1, 'Block admin')
  mockApi({
    'GET /projects/1/edge-rules': () => jsonResponse({ rules: [current] }),
    'GET /projects/1/environments': () => jsonResponse([]),
    'PATCH /projects/1/edge-rules/5': async (request) => {
      patches.push((await request.json()) as Record<string, unknown>)
      return jsonResponse(
        { title: 'Conflict', status: 409, detail: 'Edge rule 5 changed' },
        409
      )
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/projects/web/settings/edge-rules/5']}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/projects/web/settings/edge-rules/:ruleId"
              element={<EdgeRuleEditor project={project} />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const save = await screen.findByRole('button', { name: 'Save rule' })

  // The list's background refetch picks up the other save while this form
  // is open.
  current = apiRule(2, 'Block admin, edited elsewhere')
  await act(async () => {
    await client.refetchQueries({ queryKey: edgeRulesQueryKey(1) })
  })
  await waitFor(() =>
    expect(
      client.getQueryData<Array<{ revision: number }>>(
        edgeRulesQueryKey(1)
      )?.[0]?.revision
    ).toBe(2)
  )

  await act(async () => {
    fireEvent.click(save)
  })
  await waitFor(() => expect(patches).toHaveLength(1))
  // The server sees the stale revision and answers 409 instead of letting
  // this form overwrite the other change.
  expect(patches[0].revision).toBe(1)
  expect(screen.getByRole('button', { name: 'Save rule' })).toBeTruthy()
})
