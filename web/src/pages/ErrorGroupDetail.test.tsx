// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ContextType } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { AiAssistantContext } from '@/components/ai/AiAssistantContext-shared'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ErrorGroupDetail } from './ErrorGroupDetail'

installBrowserWindow()
afterEach(cleanup)

const assistant = {
  setPageContext: () => {},
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
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/errors/204']}>
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
                        { id: 7, slug: 'example' } as unknown as ProjectResponse
                      }
                    />
                  }
                />
              </Routes>
            </BreadcrumbProvider>
          </AiAssistantContext.Provider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )

  const actions = within(
    await view.findByRole('group', { name: 'Error actions' })
  )
  expect(actions.getByRole('button', { name: /Unassigned/ })).toBeTruthy()
  expect(actions.getByRole('button', { name: /Resolve/ })).toBeTruthy()
  expect(actions.getByRole('button', { name: /Ignore/ })).toBeTruthy()
})
