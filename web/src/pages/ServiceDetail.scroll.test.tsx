// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  CONSOLE_MAX_SCROLL,
  ConsoleShell,
  NEAR_THE_END,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ServiceDetail } from './ServiceDetail'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

test('a #backups link scrolls the page to the Backups card and leaves the shell in place', async () => {
  mockApi({
    'GET /external-services/3': () =>
      jsonResponse({
        sensitive_parameters: [],
        current_parameters: {},
        service: {
          id: 3,
          name: 'my-app-db',
          service_type: 'postgres',
          status: 'running',
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:00:00Z',
        },
      }),
  })
  // Backups is the last card on the page.
  layout.place('#backups', NEAR_THE_END)
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/storage/3#backups']}>
        <BreadcrumbProvider>
          <ConsoleShell>
            <Routes>
              <Route path="/storage/:id" element={<ServiceDetail />} />
            </Routes>
          </ConsoleShell>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByRole('heading', { name: 'my-app-db' })
  await waitFor(() => expect(layout.scrolls.length).toBeGreaterThan(0))
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})
