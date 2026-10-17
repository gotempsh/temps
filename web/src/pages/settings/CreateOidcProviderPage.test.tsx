// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { CreateOidcProviderPage } from './CreateOidcProviderPage'

installBrowserWindow()
afterEach(cleanup)

const REQUIRED = [
  ['oidc-name', 'Name is required'],
  ['oidc-issuer', 'Issuer URL is required'],
  ['oidc-client-id', 'Client ID is required'],
  ['oidc-client-secret', 'Client secret is required'],
] as const

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/settings/auth/new']}>
          <CreateOidcProviderPage />
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
}

function input(view: ReturnType<typeof renderPage>, id: string) {
  return view.container.querySelector<HTMLInputElement>(`#${id}`)!
}

test('an empty save marks all four required fields invalid with their own message', () => {
  const api = mockApi({})
  const view = renderPage()

  fireEvent.click(view.getByRole('button', { name: 'Save' }))

  for (const [id, message] of REQUIRED) {
    const field = input(view, id)
    expect(field.getAttribute('aria-invalid')).toBe('true')
    const described = field.getAttribute('aria-describedby')
    expect(described).toBeTruthy()
    expect(document.getElementById(described!)?.textContent).toBe(message)
  }
  expect(document.activeElement).toBe(input(view, 'oidc-name'))
  expect(api.calls.some((call) => call.method === 'POST')).toBe(false)
  api.restore()
})

test('only the still-empty fields stay invalid, and focus follows the first', () => {
  const api = mockApi({})
  const view = renderPage()
  fireEvent.change(input(view, 'oidc-name'), { target: { value: 'Corp SSO' } })
  fireEvent.change(input(view, 'oidc-client-id'), {
    target: { value: 'client-1' },
  })

  fireEvent.click(view.getByRole('button', { name: 'Save' }))

  expect(input(view, 'oidc-name').getAttribute('aria-invalid')).toBeNull()
  expect(input(view, 'oidc-client-id').getAttribute('aria-invalid')).toBeNull()
  expect(input(view, 'oidc-issuer').getAttribute('aria-invalid')).toBe('true')
  expect(input(view, 'oidc-client-secret').getAttribute('aria-invalid')).toBe(
    'true'
  )
  expect(document.activeElement).toBe(input(view, 'oidc-issuer'))

  fireEvent.change(input(view, 'oidc-issuer'), {
    target: { value: 'https://idp.example.com' },
  })
  expect(input(view, 'oidc-issuer').getAttribute('aria-invalid')).toBeNull()
  expect(view.queryByText('Issuer URL is required')).toBeNull()
  expect(view.getByText('Client secret is required')).toBeTruthy()
  api.restore()
})
