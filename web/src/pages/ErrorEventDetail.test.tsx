// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ContextType } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import {
  createPageSubjectStore,
  PageSubjectStoreContext,
} from '@/components/agent-handoff/page-subject'
import { AiAssistantContext } from '@/components/ai/AiAssistantContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ErrorEventDetail } from './ErrorEventDetail'

installBrowserWindow()
afterEach(cleanup)

const assistant = {
  setPageContext: () => {},
} as unknown as ContextType<typeof AiAssistantContext>

function mount(path: string) {
  const subjects = createPageSubjectStore()
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[path]}>
        <PageSubjectStoreContext.Provider value={subjects}>
          <AiAssistantContext.Provider value={assistant}>
            <BreadcrumbProvider>
              <Routes>
                <Route
                  path="/projects/:projectSlug/errors/:errorGroupId/event/:eventId"
                  element={
                    <ErrorEventDetail
                      project={
                        {
                          id: 7,
                          slug: 'example',
                          name: 'Example',
                        } as unknown as ProjectResponse
                      }
                    />
                  }
                />
              </Routes>
            </BreadcrumbProvider>
          </AiAssistantContext.Provider>
        </PageSubjectStoreContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, subjects }
}

const occurrence = {
  id: 1,
  error_group_id: 3,
  timestamp: '2026-11-01T10:00:00Z',
  created_at: '2026-11-01T10:00:00Z',
  source: 'sentry',
  data: {},
}

test('hands off the occurrence that loaded, by its own IDs', async () => {
  // `parseInt('1e2')` is 1, which is what the page loads; `Number('1e2')` is
  // 100. The handoff must name occurrence 1 of group 3.
  mockApi({
    'GET /projects/7/error-groups/3/events/1': () => jsonResponse(occurrence),
  })
  const { subjects } = mount('/projects/example/errors/3/event/1e2')
  await waitFor(() =>
    expect(subjects.get()).toEqual({
      kind: 'error_group',
      project: { id: 7, slug: 'example', name: 'Example' },
      groupId: 3,
      eventId: 1,
    })
  )
})

test('declares nothing while the occurrence has not loaded', async () => {
  mockApi({
    'GET /projects/7/error-groups/3/events/1': () =>
      jsonResponse({ title: 'Unavailable' }, 503),
  })
  const { subjects } = mount('/projects/example/errors/3/event/1')
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(subjects.get()).toBeNull()
})
