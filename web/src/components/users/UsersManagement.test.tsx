// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { UsersManagement } from './UsersManagement'

function render(role: string, error?: unknown) {
  const user: UserResponse = {
    id: 3,
    name: 'Example User',
    username: 'example',
    avatar_url: '',
    mfa_enabled: false,
    role,
  }
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
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
            users={error ? undefined : []}
            isLoading={false}
            error={error}
            reloadUsers={() => {}}
            onEditUser={() => {}}
          />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** Opening tags of every `<button>` whose content mentions `label`. */
function buttonTags(html: string, label: string): string[] {
  const matches = html.matchAll(
    new RegExp(`<button[^>]*>(?:(?!</button>).)*${label}`, 'gs')
  )
  return [...matches].map((m) => /<button[^>]*>/.exec(m[0])?.[0] ?? '')
}

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'This operation requires the users:read permission',
}

test('a non-admin is offered no Add User action and is told why', () => {
  const html = render('user', refused)
  expect(buttonTags(html, 'Add User')).toHaveLength(0)
  expect(html).toContain('Only administrators can add, edit or remove users')
  expect(html).toContain('Users: access denied')
  expect(html).not.toContain('Retry')
})

test('a non-admin with a verified-empty list is still offered no Add User action', () => {
  const html = render('user')
  expect(buttonTags(html, 'Add User')).toHaveLength(0)
  expect(html).toContain('data-admin-only-note')
})

test('an administrator keeps every Add User control', () => {
  const html = render('admin')
  const tags = buttonTags(html, 'Add User')
  expect(tags.length).toBeGreaterThan(0)
  expect(html).not.toContain('data-admin-only-note')
})
