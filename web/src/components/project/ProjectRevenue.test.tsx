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
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectRevenue } from './ProjectRevenue'

installBrowserWindow()
afterEach(cleanup)

test('Refresh also reloads the connected providers and their last event', async () => {
  let integrationReads = 0
  mockApi({
    'GET /projects/17/revenue/integrations': () => {
      integrationReads++
      return jsonResponse([
        {
          id: 1,
          project_id: 17,
          provider: 'stripe',
          status: 'active',
          has_secret: true,
          created_at: '2026-01-01T00:00:00Z',
          webhook_path: '/webhooks/revenue/1',
        },
      ])
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
      <MemoryRouter>
        <BreadcrumbProvider>
          <ProjectRevenue
            project={{ id: 17, slug: 'demo' } as ProjectResponse}
          />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  fireEvent.click(await screen.findByRole('button', { name: 'Refresh' }))

  await waitFor(() => expect(integrationReads).toBe(2))
})
