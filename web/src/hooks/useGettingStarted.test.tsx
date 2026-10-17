// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { useGettingStarted } from './useGettingStarted'

function Probe() {
  const { items, totalCount } = useGettingStarted()
  return (
    <p>
      {totalCount}:{items.map((item) => item.key).join(',')}
    </p>
  )
}

function renderAs(role: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Example User',
              username: 'example',
              avatar_url: '',
              mfa_enabled: false,
              role,
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <Probe />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

test('an administrator is checked against every step', () => {
  const html = renderAs('admin')
  expect(html).toContain('9:')
  expect(html).toContain('dns')
  expect(html).toContain('team')
})

test('steps a role cannot read are left out instead of counted as not done', () => {
  const html = renderAs('user')
  expect(html).toContain('7:')
  expect(html).not.toContain('dns')
  expect(html).not.toContain('team')
})
