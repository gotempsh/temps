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
import { NodesPage } from './NodesPage'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

test('a link to an advanced card opens it and scrolls the page to it, not the shell', async () => {
  mockApi({
    'GET /internal/nodes': () => jsonResponse({ nodes: [], total: 0 }),
  })
  // Cluster DNS is the last card in the advanced section, at the page's end.
  layout.place('#cluster-dns', NEAR_THE_END)
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/settings/nodes#cluster-dns']}>
        <BreadcrumbProvider>
          <ConsoleShell>
            <NodesPage />
          </ConsoleShell>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await waitFor(() =>
    expect(view.container.querySelector('#cluster-dns')).not.toBeNull()
  )
  await waitFor(() => expect(layout.scrolls.length).toBeGreaterThan(0))
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})
