// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { GitSources } from './GitSources'

installBrowserWindow()
afterEach(cleanup)

const PROVIDERS = [
  {
    id: 7,
    name: 'Internal GitLab',
    provider_type: 'gitlab',
    auth_method: 'pat',
    base_url: null,
    is_active: true,
    is_default: false,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  },
  {
    id: 8,
    name: 'Team GitHub',
    provider_type: 'github',
    auth_method: 'app',
    base_url: null,
    is_active: true,
    is_default: true,
    created_at: '2026-01-02T00:00:00Z',
    updated_at: '2026-01-02T00:00:00Z',
  },
]

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/git-providers']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/git-providers" element={<GitSources />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

// A list whose items are `role="button"` containing the row's own action menu
// is both an invalid list (axe aria-required-children) and a control nested in
// a control (axe nested-interactive).
test('provider rows are plain list items with one link and a separate menu', async () => {
  const api = mockApi({
    'GET /git-providers': () => jsonResponse(PROVIDERS),
  })
  const view = renderPage()

  const list = await view.findByRole('list')
  const rows = within(list).getAllByRole('listitem')
  expect(rows).toHaveLength(2)

  for (const [index, row] of rows.entries()) {
    expect(row.getAttribute('role')).toBeNull()
    expect(row.hasAttribute('tabindex')).toBe(false)
    const link = within(row).getByRole('link', { name: PROVIDERS[index]!.name })
    expect(link.getAttribute('href')).toBe(
      `/git-providers/${PROVIDERS[index]!.id}`
    )
    // The actions menu is a sibling control, not a descendant of a button.
    expect(row.closest('[role="button"]')).toBeNull()
    expect(within(row).getByRole('button', { name: 'Open menu' })).toBeTruthy()
  }
  api.restore()
})
