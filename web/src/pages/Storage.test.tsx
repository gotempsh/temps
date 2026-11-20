// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { Storage } from './Storage'

installBrowserWindow()
afterEach(cleanup)

function CurrentSearch() {
  return <output aria-label="search">{useLocation().search}</output>
}

function renderPage(
  url = '/storage',
  routes: Parameters<typeof mockApi>[0] = {}
) {
  // Unless a test says otherwise every request answers 404: most tests are
  // about the page's tabs, not what each tab loads.
  mockApi(routes)
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

test('a failed Refresh keeps the service cards and says the list may be stale', async () => {
  let calls = 0
  const view = renderPage('/storage', {
    'GET /external-services': () =>
      ++calls === 1
        ? jsonResponse([
            {
              id: 3,
              name: 'orders-db',
              service_type: 'postgres',
              status: 'running',
              created_at: '2026-01-01T00:00:00Z',
              updated_at: '2026-01-01T00:00:00Z',
            },
          ])
        : jsonResponse({ title: 'Internal Server Error', status: 500 }, 500),
  })
  await view.findByText('orders-db')

  fireEvent.click(view.getByRole('button', { name: 'Refresh' }))

  await view.findByText("Couldn't refresh services")
  expect(view.getByText('orders-db')).toBeTruthy()
  expect(view.queryByText('Failed to load services')).toBeNull()
})

test('a failed Refresh over an empty list says so, not only that there are no services', async () => {
  let calls = 0
  const view = renderPage('/storage', {
    'GET /external-services': () =>
      ++calls === 1
        ? jsonResponse([])
        : jsonResponse({ title: 'Internal Server Error', status: 500 }, 500),
  })
  await waitFor(() => expect(calls).toBe(1))
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(view.getByRole('button', { name: 'Refresh' }))

  await view.findByText("Couldn't refresh services")
  expect(view.queryByText('Failed to load services')).toBeNull()
})
