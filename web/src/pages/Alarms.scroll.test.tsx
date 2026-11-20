// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
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
import { Alarms } from './Alarms'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const ALARM = {
  id: 9,
  project_id: 1,
  alarm_type: 'container_restart',
  severity: 'critical',
  status: 'firing',
  title: 'Container restarting',
  message: 'boom',
  fired_at: '2026-11-01T10:00:00Z',
  created_at: '2026-11-01T10:00:00Z',
  metadata: {},
}

test('an ?alarm_id= link centres its row in the page and leaves the shell in place', async () => {
  mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [{ id: 1, slug: 'my-app', name: 'my-app' }],
        total: 1,
        page: 1,
        per_page: 100,
      }),
    'GET /projects/1/alarms/summary': () =>
      jsonResponse({
        acknowledged: 0,
        by_type: { container_restart: 1 },
        critical: 1,
        firing: 1,
        total_active: 1,
        warning: 0,
      }),
    'GET /projects/1/alarms': () =>
      jsonResponse({ items: [ALARM], page: 1, page_size: 25, total: 1 }),
  })
  // The last row of a long list: centring it asks for more than the page
  // can scroll.
  layout.place('[data-alarm-id="9"]', NEAR_THE_END)
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/alarms?alarm_id=9']}>
          <ConsoleShell>
            <Alarms embedded />
          </ConsoleShell>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
  await waitFor(() => expect(layout.scrolls.length).toBeGreaterThan(0))
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})
