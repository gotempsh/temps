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
import { Toaster, toast } from 'sonner'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { DnsProviders } from './DnsProviders'

installBrowserWindow()
afterEach(() => {
  cleanup()
  // Sonner keeps its toasts in a module-level store shared by every test.
  toast.dismiss()
})

const PROVIDER = {
  id: 4,
  name: 'Example DNS',
  provider_type: 'cloudflare',
  description: null,
  is_active: true,
  flat_hostnames_supported: false,
  credentials: {},
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

function signedInAs(role: string): UserResponse {
  return {
    id: 3,
    name: 'Example User',
    username: 'example',
    avatar_url: '',
    mfa_enabled: false,
    role,
  }
}

function renderPage(role: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: signedInAs(role),
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <DnsProviders />
          </BreadcrumbProvider>
          <Toaster />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

// Only the provider list counts: the page also loads the Cloudflare Access
// tokens card, which a provider delete does not refresh.
function listFetches(api: ReturnType<typeof mockApi>) {
  return api.calls.filter(
    (c) => c.method === 'GET' && c.path === '/dns-providers'
  ).length
}

async function openRowMenu(view: ReturnType<typeof render>) {
  const trigger = await view.findByLabelText('Open menu')
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
}

test('an administrator can reach Delete Provider from the row menu', async () => {
  const api = mockApi({
    'GET /dns-providers': () => jsonResponse([PROVIDER]),
  })
  const view = renderPage('admin')
  await openRowMenu(view)
  expect(await view.findByText('View Details')).toBeTruthy()
  expect(view.queryByText('Test Connection')).not.toBeNull()
  expect(view.queryByText('Delete Provider')).not.toBeNull()
  api.restore()
})

test('a restricted user sees View Details but neither Test Connection nor Delete Provider', async () => {
  const api = mockApi({
    'GET /dns-providers': () => jsonResponse([PROVIDER]),
  })
  const view = renderPage('user')
  await openRowMenu(view)
  expect(await view.findByText('View Details')).toBeTruthy()
  expect(view.queryByText('Test Connection')).toBeNull()
  expect(view.queryByText('Delete Provider')).toBeNull()
  api.restore()
})

test('a refused delete keeps the confirmation open and the list unrefreshed', async () => {
  const api = mockApi({
    'GET /dns-providers': () => jsonResponse([PROVIDER]),
    'DELETE /dns-providers/4': () =>
      jsonResponse(
        {
          title: 'Forbidden',
          status: 403,
          detail: 'Requires dns_providers:write permission',
        },
        403
      ),
  })
  const view = renderPage('admin')
  await openRowMenu(view)
  await act(async () => {
    fireEvent.click(await view.findByText('Delete Provider'))
  })
  const confirm = await view.findByRole('button', { name: 'Delete Provider' })
  await act(async () => {
    fireEvent.click(confirm)
  })
  await waitFor(() =>
    expect(api.called('DELETE', '/dns-providers/4')).toBe(true)
  )

  // A refusal is not a deletion: the dialog stays for a retry and the list is
  // not refetched as though something changed.
  await waitFor(() =>
    expect(view.getByText('Delete DNS provider Example DNS?')).toBeTruthy()
  )
  expect(listFetches(api)).toBe(1)

  // The failure is reported with the server's own reason, never as a success.
  expect(await view.findByText('Failed to delete DNS provider')).toBeTruthy()
  expect(
    await view.findByText('Requires dns_providers:write permission')
  ).toBeTruthy()
  expect(view.queryByText('DNS provider deleted successfully')).toBeNull()
  api.restore()
})

test('a delete the server accepts closes the confirmation and refreshes the list', async () => {
  let deleted = false
  const api = mockApi({
    'GET /dns-providers': () => jsonResponse(deleted ? [] : [PROVIDER]),
    'DELETE /dns-providers/4': () => {
      deleted = true
      return jsonResponse({})
    },
  })
  const view = renderPage('admin')
  await openRowMenu(view)
  await act(async () => {
    fireEvent.click(await view.findByText('Delete Provider'))
  })
  await act(async () => {
    fireEvent.click(
      await view.findByRole('button', { name: 'Delete Provider' })
    )
  })
  await waitFor(() =>
    expect(view.queryByText('Delete DNS provider Example DNS?')).toBeNull()
  )
  await waitFor(() => expect(listFetches(api)).toBe(2))
  expect(
    await view.findByText('DNS provider deleted successfully')
  ).toBeTruthy()
  api.restore()
})

test('losing write access while the confirmation is open closes it for good', async () => {
  const api = mockApi({
    'GET /dns-providers': () => jsonResponse([PROVIDER]),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const page = (role: string) => (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: signedInAs(role),
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <DnsProviders />
          </BreadcrumbProvider>
          <Toaster />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const view = render(page('admin'))
  await openRowMenu(view)
  await act(async () => {
    fireEvent.click(await view.findByText('Delete Provider'))
  })
  expect(await view.findByText('Delete DNS provider Example DNS?')).toBeTruthy()

  view.rerender(page('user'))
  await waitFor(() =>
    expect(view.queryByText('Delete DNS provider Example DNS?')).toBeNull()
  )
  expect(api.called('DELETE', '/dns-providers/4')).toBe(false)

  // Restoring the role must not bring the old confirmation back.
  view.rerender(page('admin'))
  await act(async () => {})
  expect(view.queryByText('Delete DNS provider Example DNS?')).toBeNull()
  expect(view.getByText('Example DNS')).toBeTruthy()
  api.restore()
})
