// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from '@/components/ai/AiAssistantContext-shared'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ErrorTracking } from './ErrorTracking'

installBrowserWindow()
afterEach(cleanup)

test('Refresh also reloads the deployment markers on the error chart', async () => {
  let deploymentReads = 0
  mockApi({
    'GET /projects/17/has-error-groups': () =>
      jsonResponse({ has_error_groups: true }),
    'GET /projects/17/environments': () => jsonResponse([]),
    'GET /projects/17/error-groups': () =>
      jsonResponse({
        data: [],
        pagination: { page: 1, page_size: 20, total_count: 0, total_pages: 0 },
      }),
    'GET /projects/17/error-dashboard-stats': () =>
      jsonResponse({
        start_time: '2026-01-01T00:00:00Z',
        end_time: '2026-01-02T00:00:00Z',
        error_groups: 0,
        error_groups_previous_period: 0,
        total_errors: 0,
        total_errors_change_percent: 0,
        total_errors_previous_period: 0,
      }),
    'GET /projects/17/deployments': () => {
      deploymentReads++
      return jsonResponse({ deployments: [], total: 0, page: 1, per_page: 50 })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false, staleTime: Infinity } },
        })
      }
    >
      <MemoryRouter
        initialEntries={[
          '/errors?filter=custom&from=2026-01-01T00:00:00.000Z&to=2026-01-02T00:00:00.000Z',
        ]}
      >
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Operator',
              username: 'operator',
              role: 'admin',
              avatar_url: '',
              mfa_enabled: false,
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <ErrorTracking
            project={{ id: 17, slug: 'demo' } as ProjectResponse}
          />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await waitFor(() => expect(deploymentReads).toBe(1))

  fireEvent.click(await screen.findByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(deploymentReads).toBe(2))
})

function group(id: number) {
  return {
    id,
    project_id: 17,
    title: `TypeError in checkout step ${id}`,
    error_type: 'TypeError',
    status: 'unresolved',
    total_count: 3,
    first_seen: '2026-01-01T00:00:00Z',
    last_seen: '2026-01-01T12:00:00Z',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T12:00:00Z',
  }
}

test('a refresh that leaves the page past the end goes back to a page with groups', async () => {
  const pages: string[] = []
  let refreshed = false
  mockApi({
    'GET /projects/17/has-error-groups': () =>
      jsonResponse({ has_error_groups: true }),
    'GET /projects/17/environments': () => jsonResponse([]),
    'GET /projects/17/error-groups': (request) => {
      const page = new URL(request.url).searchParams.get('page')!
      pages.push(page)
      // Groups last seen before the moved window's start drop out.
      const total = refreshed ? 20 : 41
      const totalPages = Math.ceil(total / 20)
      return jsonResponse({
        data: Number(page) > totalPages ? [] : [group(Number(page))],
        pagination: {
          page: Number(page),
          page_size: 20,
          total_count: total,
          total_pages: totalPages,
        },
      })
    },
    'GET /projects/17/error-dashboard-stats': () =>
      jsonResponse({
        start_time: '2026-01-01T00:00:00Z',
        end_time: '2026-01-02T00:00:00Z',
        error_groups: 0,
        error_groups_previous_period: 0,
        total_errors: 0,
        total_errors_change_percent: 0,
        total_errors_previous_period: 0,
      }),
    'GET /projects/17/deployments': () =>
      jsonResponse({ deployments: [], total: 0, page: 1, per_page: 50 }),
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false, staleTime: Infinity } },
        })
      }
    >
      <MemoryRouter initialEntries={['/errors?filter=24hours']}>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Operator',
              username: 'operator',
              role: 'admin',
              avatar_url: '',
              mfa_enabled: false,
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <AiAssistantContext.Provider
            value={{ open: () => {} } as unknown as AiAssistantValue}
          >
            <ErrorTracking
              project={{ id: 17, slug: 'demo' } as ProjectResponse}
            />
          </AiAssistantContext.Provider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByText('TypeError in checkout step 1')
  fireEvent.click(screen.getByRole('button', { name: 'Next' }))
  await screen.findByText('TypeError in checkout step 2')
  fireEvent.click(screen.getByRole('button', { name: 'Next' }))
  await screen.findByText('TypeError in checkout step 3')

  refreshed = true
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await screen.findByText('TypeError in checkout step 1')
  expect(pages.slice(-2)).toEqual(['3', '1'])
})
