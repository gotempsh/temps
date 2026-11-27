// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { Welcome } from './Welcome'

installBrowserWindow()
afterEach(cleanup)

const user = (role: string): UserResponse => ({
  id: 3,
  name: 'Example User',
  username: 'example',
  avatar_url: '',
  mfa_enabled: false,
  role,
})

test('a non-admin on /welcome is told setup is for administrators, not left loading', () => {
  const api = mockApi({})
  try {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={['/welcome']}>
          <AuthContext.Provider
            value={{
              user: user('user'),
              isLoading: false,
              error: null,
              logout: async () => {},
              refetch: () => {},
            }}
          >
            <BreadcrumbProvider>
              <Welcome />
            </BreadcrumbProvider>
          </AuthContext.Provider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(view.queryByRole('status', { name: 'Loading setup' })).toBeNull()
    expect(view.getByRole('note').textContent).toContain(
      'Only administrators can set up this install.'
    )
    // Skip setup is an administrator's write; it is not offered.
    expect(view.queryByRole('button', { name: 'Skip setup' })).toBeNull()
    // Nothing is asked of the server on the administrator's behalf.
    expect(api.calls).toEqual([])
  } finally {
    api.restore()
  }
})
