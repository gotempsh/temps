// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { Login } from './Login'

installBrowserWindow()

const unhandled: unknown[] = []
process.on('unhandledRejection', (reason) => unhandled.push(reason))
afterEach(() => {
  cleanup()
  unhandled.length = 0
})

test('a failed sign-in does not raise an unhandled promise rejection', async () => {
  const api = mockApi({
    'GET /auth/email-status': () =>
      jsonResponse({ oidc_providers: [], password_reset_available: false }),
    'POST /auth/login': () =>
      jsonResponse(
        { title: 'Unauthorized', detail: 'Invalid credentials' },
        401
      ),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const auth = {
    user: null,
    isLoading: false,
    error: null,
    logout: async () => {},
    refetch: () => {},
  }
  const view = render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider value={auth}>
        <MemoryRouter>
          <Login />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )

  fireEvent.change(view.getByPlaceholderText('you@example.com'), {
    target: { value: 'person@example.test' },
  })
  fireEvent.change(view.getByPlaceholderText('Enter your password'), {
    target: { value: 'not-the-password' },
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Sign in' }))
  })

  await waitFor(() => expect(api.called('POST', '/auth/login')).toBe(true))
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
  expect(unhandled).toHaveLength(0)
  // The form is usable again after the failure.
  await waitFor(() =>
    expect(
      (view.getByRole('button', { name: 'Sign in' }) as HTMLButtonElement)
        .disabled
    ).toBe(false)
  )
  api.restore()
})
