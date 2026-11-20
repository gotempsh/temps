// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import type { GetProjectsResponse, UserResponse } from '@/api/client'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  CONSOLE_MAX_SCROLL,
  ConsoleShell,
  NEAR_THE_END,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { mockApi } from '@/test/fetch-mock'
import { EmailDomainDetail } from './EmailDomainDetail'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const DOMAIN = {
  id: 7,
  domain: 'mail.example.com',
  status: 'verified',
  provider_id: 5,
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}
const PROVIDER = {
  id: 5,
  name: 'Primary relay',
  provider_type: 'ses',
  region: 'eu-test-1',
  is_active: true,
  credentials: {},
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}

function renderDomain() {
  mockApi({})
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(['email-domain', 7], { domain: DOMAIN, dns_records: [] })
  client.setQueryData(['email-stats', 7], {
    total: 0,
    sent: 0,
    failed: 0,
    captured: 0,
    queued: 0,
    sending: 0,
    delivery_unknown: 0,
  })
  client.setQueryData(['email-providers'], [PROVIDER])
  client.setQueryData(['email-domains'], [DOMAIN])
  client.setQueryData(['dns-providers'], [])
  client.setQueryData(['email-domain-projects', 7], [])
  client.setQueryData(
    getProjectsOptions({ query: { page: 1, per_page: 100 } }).queryKey,
    {
      projects: [{ id: 1, slug: 'my-app', name: 'my-app' }],
      total: 1,
      page: 1,
      per_page: 100,
    } as unknown as GetProjectsResponse
  )
  const user = { id: 1, role: 'admin' } as unknown as UserResponse
  return render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter initialEntries={['/email/domains/7']}>
          <BreadcrumbProvider>
            <ConsoleShell>
              <Routes>
                <Route
                  path="/email/domains/:id"
                  element={<EmailDomainDetail />}
                />
              </Routes>
            </ConsoleShell>
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('"Authorize a project" scrolls the page to the authorized projects, not the shell', async () => {
  // On a phone the panel sits below the main column, at the end of the page.
  layout.place('#authorized-projects', NEAR_THE_END)
  const view = renderDomain()
  fireEvent.click(
    await view.findByRole('button', { name: 'Authorize a project' })
  )
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})
