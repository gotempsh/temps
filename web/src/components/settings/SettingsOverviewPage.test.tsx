// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, render, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  AuthContext,
  type AuthContextType,
} from '@/contexts/AuthContext-shared'
import { PageNamedByChromeContext } from '@temps-sdk/ds'
import { PageHeader } from '@/components/layout/PageContainer'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { SettingsLayout } from './SettingsLayout'
import { SettingsOverviewPage } from './SettingsOverviewPage'

installBrowserWindow()
afterEach(cleanup)

function renderSettings(path: string, role: string) {
  mockApi({})
  const auth = {
    user: { role },
    isLoading: false,
    error: null,
    logout: async () => undefined,
    refetch: () => undefined,
  } as unknown as AuthContextType
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AuthContext.Provider value={auth}>
        <MemoryRouter initialEntries={[path]}>
          <BreadcrumbProvider>
            <Routes>
              <Route path="/settings" element={<SettingsLayout />}>
                <Route path="overview" element={<SettingsOverviewPage />} />
                <Route
                  path="*"
                  element={<PageHeader title="A settings page" />}
                />
              </Route>
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

describe('the Settings overview', () => {
  test('lists every settings page under its group, each a link', () => {
    const view = renderSettings('/settings/overview', 'admin')
    expect(view.getByRole('heading', { level: 1 }).textContent).toBe('Settings')
    expect(
      view.getAllByRole('heading', { level: 2 }).map((h) => h.textContent)
    ).toEqual(['Platform', 'AI', 'Access', 'Security', 'Instance', 'Data'])
    const access = within(view.getByRole('region', { name: 'Access' }))
    expect(
      access
        .getAllByRole('link')
        .map((a) => [a.textContent, a.getAttribute('href')])
    ).toEqual([
      ['Users', '/settings/users'],
      ['Teams', '/settings/teams'],
      ['Sign-in', '/settings/auth'],
      ['API keys', '/settings/keys'],
      ['Audit log', '/audit-logs'],
    ])
    expect(access.getByText('Who changed what, and when.')).toBeTruthy()
    const data = within(view.getByRole('region', { name: 'Data' }))
    expect(
      data.getByRole('link', { name: 'Extensions' }).getAttribute('href')
    ).toBe('/settings/plugins')
  })

  test('leaves the audit log out for a role that cannot read it', () => {
    const view = renderSettings('/settings/overview', 'reader')
    expect(view.queryByRole('link', { name: 'Audit log' })).toBeNull()
    expect(view.getByRole('link', { name: 'Users' })).toBeTruthy()
  })
})

describe('a settings entry with several pages', () => {
  const tabs = (root: HTMLElement) =>
    [...root.querySelectorAll('nav a')].map((a) => [
      a.textContent,
      a.getAttribute('href'),
    ])

  test('shows them as tabs, with the page shown selected', () => {
    const view = renderSettings('/settings/request-timeouts', 'admin')
    expect(view.getByRole('navigation', { name: 'Limits pages' })).toBeTruthy()
    expect(tabs(view.container)).toEqual([
      ['Builds', '/settings/build-limits'],
      ['Request timeouts', '/settings/request-timeouts'],
      ['Health checks', '/settings/service-healthchecks'],
    ])
    expect(
      view.container.querySelector('nav [aria-current="page"]')?.textContent
    ).toBe('Request timeouts')
    // The tab names the page; the title stays for assistive technology.
    expect(view.getByRole('heading', { level: 1 }).className).toContain(
      'sr-only'
    )
  })

  test('a strip above the layout still names the page', () => {
    // Notification channels and the MCP server keep a /settings URL but sit
    // under a tab strip of the server menu, outside this layout.
    mockApi({})
    const view = render(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter initialEntries={['/settings/notifications']}>
          <PageNamedByChromeContext.Provider value={true}>
            <Routes>
              <Route path="/settings" element={<SettingsLayout />}>
                <Route path="*" element={<PageHeader title="Channels" />} />
              </Route>
            </Routes>
          </PageNamedByChromeContext.Provider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(view.getByRole('heading', { level: 1 }).className).toContain(
      'sr-only'
    )
  })

  test('a page of its own has no strip and shows its title', () => {
    const view = renderSettings('/settings/users', 'admin')
    expect(view.container.querySelector('nav')).toBeNull()
    expect(view.getByRole('heading', { level: 1 }).className).not.toContain(
      'sr-only'
    )
  })
})
