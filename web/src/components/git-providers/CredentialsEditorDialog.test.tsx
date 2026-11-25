// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProviderResponse } from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CredentialsEditorDialog } from './CredentialsEditorDialog'

afterEach(cleanup)

const provider = {
  id: 4,
  name: 'self-hosted git',
  provider_type: 'generic',
  auth_method: 'basic',
  base_url: null,
  is_active: true,
  is_default: false,
  created_at: '2026-11-25T11:39:26Z',
  updated_at: '2026-11-25T11:39:26Z',
} as ProviderResponse

test('saving sends only the changed fields through the generated client', async () => {
  let submitted: unknown
  mockApi({
    'PATCH /git-providers/4/credentials': async (request) => {
      submitted = await request.json()
      return jsonResponse({ ...provider, updated_fields: ['password'] })
    },
  })
  let open = true
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { mutations: { retry: false } } })
      }
    >
      <CredentialsEditorDialog
        provider={provider}
        open
        onOpenChange={(next) => {
          open = next
        }}
      />
    </QueryClientProvider>
  )

  fireEvent.change(view.getByLabelText('Password or token'), {
    target: { value: 'new-app-password' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Save Credentials' }))

  await waitFor(() => expect(open).toBe(false))
  expect(submitted).toEqual({ password: 'new-app-password' })
})

test('a failed save says why beside the buttons and keeps what was typed', async () => {
  mockApi({
    'PATCH /git-providers/4/credentials': () =>
      jsonResponse(
        {
          type: 'about:blank',
          title: 'Invalid credentials',
          status: 400,
          detail: 'The password must not contain a newline.',
        },
        400
      ),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { mutations: { retry: false } } })
      }
    >
      <CredentialsEditorDialog
        provider={provider}
        open
        onOpenChange={() => {}}
      />
    </QueryClientProvider>
  )
  const password = view.getByLabelText('Password or token') as HTMLInputElement

  fireEvent.change(password, { target: { value: 'rejected-password' } })
  fireEvent.click(view.getByRole('button', { name: 'Save Credentials' }))

  const saveError = await view.findByText(/Credentials were not saved/)
  expect(saveError.textContent).toContain(
    'The password must not contain a newline.'
  )
  expect(password.value).toBe('rejected-password')

  fireEvent.change(password, { target: { value: 'another-password' } })
  await waitFor(() =>
    expect(view.queryByText(/Credentials were not saved/)).toBeNull()
  )
})
