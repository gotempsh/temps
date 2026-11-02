// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { Storage } from './Storage'

installBrowserWindow()
afterEach(cleanup)

function CurrentSearch() {
  return <output aria-label="search">{useLocation().search}</output>
}

function renderPage(url = '/storage') {
  // Every request answers 404: these tests are about the page's tabs, not
  // what each tab loads.
  mockApi({})
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[url]}>
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
          <BreadcrumbProvider>
            <Storage />
            <CurrentSearch />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the two database pages are tabs with icons under one page title, not a sidebar', () => {
  const view = renderPage()

  expect(view.queryByRole('complementary')).toBeNull()
  expect(
    view.getAllByRole('heading', { name: 'Databases' }).length
  ).toBeGreaterThan(0)
  const tabs = view.getAllByRole('tab')
  expect(tabs.map((tab) => tab.textContent)).toEqual([
    'Databases',
    'Platform services',
  ])
  for (const tab of tabs) expect(tab.querySelector('svg')).not.toBeNull()
  expect(tabs[0].getAttribute('aria-selected')).toBe('true')
})

test('an existing ?tab=platform link opens Platform services, and switching tabs keeps the URL in step', async () => {
  const view = renderPage('/storage?tab=platform')

  const platform = view.getByRole('tab', { name: 'Platform services' })
  expect(platform.getAttribute('aria-selected')).toBe('true')
  // The tab's explanation lives in the page header, not a repeated alert.
  view.getByText(/Shared by every deployed service/)
  expect(view.queryByText('Platform Services')).toBeNull()

  fireEvent.mouseDown(view.getByRole('tab', { name: 'Databases' }), {
    button: 0,
  })
  await waitFor(() =>
    expect(view.getByLabelText('search').textContent).toBe('?tab=external')
  )
  expect(view.queryByText(/Shared by every deployed service/)).toBeNull()
})
