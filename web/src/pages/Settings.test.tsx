// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client'
import type { PlatformSettings } from '@/api/platformSettings'
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
import { Settings } from './Settings'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const SETTINGS = {
  external_url: 'https://console.example.com',
  preview_domain: 'localho.st',
  letsencrypt: { email: 'dev@example.com', environment: 'production' },
} as PlatformSettings

function renderSettings(entry: string) {
  mockApi({})
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(['platform-settings'], SETTINGS)
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
        <MemoryRouter initialEntries={[entry]}>
          <BreadcrumbProvider>
            <ConsoleShell>
              <Settings />
            </ConsoleShell>
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('a #public-ingress link centres the field in the page and leaves the shell in place', async () => {
  // On a short screen the field sits at the end of what the page can scroll.
  layout.place('#public-ingress', NEAR_THE_END)
  const view = renderSettings('/settings#public-ingress')
  await waitFor(() =>
    expect(view.container.querySelector('#public-ingress')).not.toBeNull()
  )
  await waitFor(() => expect(layout.scrolls.length).toBeGreaterThan(0))
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})

test('without the hash the settings page does not scroll', async () => {
  layout.place('#public-ingress', NEAR_THE_END)
  const view = renderSettings('/settings')
  await waitFor(() =>
    expect(view.container.querySelector('#public-ingress')).not.toBeNull()
  )
  expect(layout.scrolls).toEqual([])
  expect(layout.intoView).toEqual([])
})
