// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  render,
  within,
  fireEvent,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { mockApi, jsonResponse } from '@/test/fetch-mock'
import { VaultLinks } from './VaultLinks'
installBrowserWindow()
afterEach(cleanup)

test('unlink requires a revocation confirmation and preserves failures', async () => {
  const api = mockApi({
    'GET /access-vaults/vault_fixture/links': () =>
      jsonResponse({
        items: [
          {
            id: 'link_fixture',
            vault_id: 'vault_fixture',
            project_id: 7,
            sandbox_id: null,
            consumer_name: 'Fixture project',
            created_at: '2026-01-01T00:00:00Z',
          },
        ],
        total: 1,
        page: 1,
        page_size: 20,
      }),
    'DELETE /access-vaults/vault_fixture/links/link_fixture': () =>
      jsonResponse({ detail: 'Storage temporarily unavailable' }, 503),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultLinks vaultId="vault_fixture" />
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Fixture project')).toBeTruthy())
    fireEvent.click(ui.getByRole('button', { name: 'Unlink' }))
    const dialog = within(ui.getByRole('dialog'))
    expect(dialog.getByText(/Live sessions and grants/)).toBeTruthy()
    expect(
      api.called('DELETE', '/access-vaults/vault_fixture/links/link_fixture')
    ).toBe(false)
    fireEvent.click(
      dialog.getByRole('button', { name: 'Unlink and revoke access' })
    )
    await waitFor(() =>
      expect(dialog.getByText('Storage temporarily unavailable')).toBeTruthy()
    )
    expect(ui.getByText('Fixture project')).toBeTruthy()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
