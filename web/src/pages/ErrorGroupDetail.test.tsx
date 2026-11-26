// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ContextType } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import {
  createPageSubjectStore,
  PageSubjectStoreContext,
} from '@/components/agent-handoff/page-subject'
import { AiAssistantContext } from '@/components/ai/AiAssistantContext-shared'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ErrorGroupDetail } from './ErrorGroupDetail'

installBrowserWindow()
afterEach(cleanup)

const assistantContexts: Array<{ value: string; label: string } | null> = []
const assistant = {
  setPageContext: (context: { value: string; label: string } | null) => {
    assistantContexts.push(context)
  },
} as unknown as ContextType<typeof AiAssistantContext>

test('assigning and resolving an error sit together as one group of actions', async () => {
  mockApi({
    'GET /projects/7/error-groups/204': () =>
      jsonResponse({
        id: 204,
        title: 'Synthetic failure',
        status: 'unresolved',
        total_count: 19,
        first_seen: '2026-10-20T10:00:00Z',
        last_seen: '2026-11-01T10:00:00Z',
        assigned_to: null,
      }),
  })
  const subjects = createPageSubjectStore()
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/errors/204']}>
        <PageSubjectStoreContext.Provider value={subjects}>
          <AuthContext.Provider
            value={
              { user: { id: 1, role: 'user' } } as unknown as NonNullable<
                ContextType<typeof AuthContext>
              >
            }
          >
            <AiAssistantContext.Provider value={assistant}>
              <BreadcrumbProvider>
                <Routes>
                  <Route
                    path="/errors/:errorGroupId"
                    element={
                      <ErrorGroupDetail
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
          </AuthContext.Provider>
        </PageSubjectStoreContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )

  const actions = within(
    await view.findByRole('group', { name: 'Error actions' })
  )
  expect(actions.getByRole('button', { name: /Unassigned/ })).toBeTruthy()
  expect(actions.getByRole('button', { name: /Resolve/ })).toBeTruthy()
  expect(actions.getByRole('button', { name: /Ignore/ })).toBeTruthy()

  // The page declares the group once, for the header's "Hand off to agent"
  // and for the built-in assistant, from the same declaration.
  await waitFor(() =>
    expect(subjects.get()).toEqual({
      kind: 'error_group',
      project: { id: 7, slug: 'example', name: 'Example' },
      groupId: 204,
    })
  )
  const told = assistantContexts.filter(Boolean).pop()
  expect(told?.label).toBe('this error')
  expect(told?.value).toContain(
    'The user is viewing error group 204 in project "Example" (ID 7) in the Temps console.'
  )
})

test('a link that reads as two numbers hands off the group on screen', async () => {
  // `parseInt('2e2')` is 2, which is what the page loads; `Number('2e2')` is
  // 200. The handoff must name group 2.
  mockApi({
    'GET /projects/7/error-groups/2': () =>
      jsonResponse({
        id: 2,
        title: 'Synthetic failure',
        status: 'unresolved',
        total_count: 3,
        first_seen: '2026-10-20T10:00:00Z',
        last_seen: '2026-11-01T10:00:00Z',
        assigned_to: null,
      }),
  })
  const subjects = createPageSubjectStore()
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/errors/2e2']}>
        <PageSubjectStoreContext.Provider value={subjects}>
          <AuthContext.Provider
            value={
              { user: { id: 1, role: 'user' } } as unknown as NonNullable<
                ContextType<typeof AuthContext>
              >
            }
          >
            <AiAssistantContext.Provider value={assistant}>
              <BreadcrumbProvider>
                <Routes>
                  <Route
                    path="/errors/:errorGroupId"
                    element={
                      <ErrorGroupDetail
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
          </AuthContext.Provider>
        </PageSubjectStoreContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await view.findByRole('group', { name: 'Error actions' })
  await waitFor(() =>
    expect(subjects.get()).toMatchObject({ kind: 'error_group', groupId: 2 })
  )
})
