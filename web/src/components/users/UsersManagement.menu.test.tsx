// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { UsersManagement } from './UsersManagement'

installBrowserWindow()
afterEach(cleanup)

const listedUser = {
  user: {
    id: 9,
    name: 'Listed Person',
    username: 'listed',
    email: 'listed@example.test',
    image: '',
    email_verified: true,
    mfa_enabled: false,
    must_change_password: false,
    created_at: 0,
    updated_at: 0,
  },
  roles: [],
}

const client = new QueryClient()

function page(role: string) {
  const user: UserResponse = {
    id: 3,
    name: 'Example User',
    username: 'example',
    avatar_url: '',
    mfa_enabled: false,
    role,
  }
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <UsersManagement
            users={[listedUser] as never}
            isLoading={false}
            error={undefined}
            reloadUsers={() => {}}
            onEditUser={() => {}}
          />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an administrator can open the row menu', async () => {
  const view = render(page('admin'))
  await act(async () => {
    fireEvent.pointerDown(view.getByRole('button', { name: 'Open menu' }), {
      button: 0,
      ctrlKey: false,
    })
  })
  expect(await view.findByText('Manage Roles')).toBeTruthy()
})

test('a row menu left open when access is revoked stays closed when it is restored', async () => {
  const view = render(page('admin'))
  await act(async () => {
    fireEvent.pointerDown(view.getByRole('button', { name: 'Open menu' }), {
      button: 0,
      ctrlKey: false,
    })
  })
  expect(await view.findByText('Manage Roles')).toBeTruthy()

  await act(async () => {
    view.rerender(page('user'))
  })
  expect(view.queryByText('Manage Roles')).toBeNull()
  expect(view.queryByRole('button', { name: 'Open menu' })).toBeNull()

  await act(async () => {
    view.rerender(page('admin'))
  })
  expect(view.getByRole('button', { name: 'Open menu' })).toBeTruthy()
  expect(view.queryByText('Manage Roles')).toBeNull()
})
